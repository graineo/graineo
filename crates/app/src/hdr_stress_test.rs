//! Model-based stress test of "HDR zusammenfügen": random sequences of user actions on a merged photo, driven
//! through the whole app without a window (library in the test run's data folder, real job threads), with the merge
//! jobs stopped at fixed points (`hdr_ui::gate`), so that an action can meet a job at every stage. After every step
//! the states that have to agree are compared: the stored recipe, the tool "Zusammenfügen", the photo shown, the
//! result cache, the jobs, the undo steps.
//!
//! - Seeds: `GRAINEO_HDR_STRESS="<sequences>x<steps>[@<first seed>]"` (default: a short run); a failure prints the
//!   seed, the actions and a shortened sequence that still fails. `GRAINEO_HDR_STRESS_SEED=<seed>` runs one sequence
//!   and prints every step.
//! - The batch "Alle Reihen zusammenfügen" has sequences of its own (second half of this file, [`Batch`]): three
//!   brackets, started through the library as the dialog does it; `GRAINEO_HDR_STRESS_BATCH="<sequences>x<steps>[@<first
//!   seed>]"`, `GRAINEO_HDR_STRESS_BATCH_SEED=<seed>`.
//! - Not covered: the pointer (clicks go through the panel's test hook at the place the widgets report them), real
//!   thread races inside a computation (jobs only stop at the four points), the dialog of the batch (its list and
//!   switches have a test of their own in `hdr_batch.rs`), the GPU, export file formats (the document of an export is
//!   checked, not the file), a real end of the process (the "quit while computing" action ends the session inside the
//!   test process: jobs are told to stop and leave no trace).
//!
//! Needs `GRAINEO_DATA_DIR`. The frames are tiny synthetic DNGs (96 × 64), a merge takes milliseconds.

use super::*;
use crate::App;
use eframe::App as _;
use std::path::Path;

/// Size of the synthetic frames (sensor pixels).
const W: usize = 96;
const H: usize = 64;

/// A tiny Bayer DNG (RGGB, 16 bit, uncompressed): a smooth scene with a few bright spots, exposed with `k`
/// (1.0 = the middle exposure), clipped at white, with noise of its own (`seed`).
pub(crate) fn tiny_dng(path: &Path, k: f64, seconds: f64, second_of_day: u32, seed: u64) {
    use develop::tiffw::{rat, srat, Ifd, Value::*};
    let (black, white) = (256.0f64, 4095.0f64);
    let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut noise = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let mut data = Vec::with_capacity(W * H * 2);
    for y in 0..H {
        for x in 0..W {
            let (u, v) = (x as f64 / W as f64, y as f64 / H as f64);
            // scene radiance per channel: a ramp over four stops, a bright window and a dark corner
            let mut l = 0.02 * (2.0f64).powf(4.0 * u) * (0.6 + 0.4 * v);
            if (0.6..0.8).contains(&u) && (0.2..0.5).contains(&v) {
                l *= 6.0;
            }
            let c = match (x % 2, y % 2) {
                (0, 0) => 0.55,
                (1, 1) => 0.7,
                _ => 1.0,
            };
            let s = l * c * k;
            let val = (black + (white - black) * s + 6.0 * noise() * (1.0 + 30.0 * s).sqrt()).clamp(0.0, white);
            data.extend_from_slice(&(val.round() as u16).to_le_bytes());
        }
    }
    let date = format!("2026:11:01 10:{:02}:{:02}", second_of_day / 60, second_of_day % 60);
    let exif = Ifd {
        tags: vec![(33434, Rational(vec![rat(seconds)])), (33437, Rational(vec![rat(8.0)])), (34855, Short(vec![100])), (36864, Undefined(b"0230".to_vec())), (36867, Ascii(date.clone())), (36868, Ascii(date.clone())), (37386, Rational(vec![rat(35.0)]))],
        ..Default::default()
    };
    let tags = vec![
        (254, Long(vec![0])),
        (256, Long(vec![W as u32])),
        (257, Long(vec![H as u32])),
        (258, Short(vec![16])),
        (259, Short(vec![1])),
        (262, Short(vec![32803])),
        (271, Ascii("Testcam".into())),
        (272, Ascii("Stress One".into())),
        (274, Short(vec![1])),
        (277, Short(vec![1])),
        (278, Long(vec![H as u32])),
        (284, Short(vec![1])),
        (306, Ascii(date)),
        (33421, Short(vec![2, 2])),
        (33422, Byte(vec![0, 1, 1, 2])),
        (50706, Byte(vec![1, 4, 0, 0])),
        (50707, Byte(vec![1, 1, 0, 0])),
        (50708, Ascii("Testcam Stress One".into())),
        (50714, Short(vec![black as u16])),
        (50717, Short(vec![white as u16])),
        (50721, SRational([0.8, -0.2, -0.1, -0.4, 1.2, 0.2, -0.1, 0.2, 0.7].iter().map(|v| srat(*v)).collect())),
        (50728, Rational(vec![rat(0.55), rat(1.0), rat(0.7)])),
        (50778, Short(vec![21])),
    ];
    let ifd = Ifd { tags, strips: vec![data], sub_ifds: Vec::new(), exif: Some(Box::new(exif)) };
    develop::tiffw::write_ifd(std::io::BufWriter::new(std::fs::File::create(path).unwrap()), ifd).unwrap();
}


// ─────────────────────────────────────────────── random numbers and actions

#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03 | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        ((self.next() >> 33) as usize) % n.max(1)
    }
}

/// One user action (or the test letting a job go on). The numbers are taken modulo what is there when it is carried
/// out; an action that is not possible then does nothing – so a sequence stays valid when steps are removed from it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    /// "HDR zusammenfügen" of a selection (bit mask of the seven frames)
    Create(usize),
    /// tick of a row of the list (frame or photo offered)
    Tick(usize),
    Reference(usize),
    Ghost(usize),
    Output(usize),
    Align,
    Strength(usize),
    Paint(usize),
    AreaFrame(usize, usize),
    Eye(usize),
    DeleteArea(usize),
    Cancel,
    Recompute,
    /// "Auswahl zurücknehmen" (the way out of a change of the frames that was not computed)
    Restore,
    Undo,
    Redo,
    /// the oldest waiting job passes one point
    JobStep(usize),
    /// every job runs to its end
    JobsFinish,
    /// show a source frame / the merged photo
    Photo(usize),
    /// "HDR entfernen"; the question about own work is answered with yes (`true`) or cancelled
    Remove(bool),
    /// a source file is not reachable any more / is back
    SourceGone(usize),
    SourceBack,
    /// a source file is damaged (it is there, it cannot be read as a RAW)
    SourceBroken(usize),
    Export,
    ClearCache,
    Restart,
    /// a develop slider moved by hand
    Develop(usize),
    /// time passes (tenths of a second)
    Wait(usize),
    /// the app is quit while whatever runs is running, and started again
    Kill,
    /// an ordinary photo comes into the library (a file appears in the folder, the sync finds it)
    NewPhoto,
    /// "Sicherung ansehen": what a process that works on another library than the real one runs on the cache at its
    /// start (tidying, the size limit) and on "Leeren" – it must not remove anything
    ViewBackup,
}

fn random_act(r: &mut Rng) -> Act {
    match r.below(100) {
        0..=5 => Act::Create(1 + r.below(127)),
        6..=14 => Act::Tick(r.below(7)),
        15..=20 => Act::Reference(r.below(7)),
        21..=27 => Act::Ghost(r.below(3)),
        28..=33 => Act::Output(r.below(2)),
        34..=37 => Act::Align,
        38..=42 => Act::Strength(r.below(101)),
        43..=47 => Act::Paint(r.below(35)),
        48..=49 => Act::AreaFrame(r.below(3), r.below(7)),
        50..=51 => Act::Eye(r.below(3)),
        52..=53 => Act::DeleteArea(r.below(3)),
        54..=57 => Act::Cancel,
        58 => Act::Recompute,
        59 => Act::Restore,
        60..=65 => Act::Undo,
        66..=68 => Act::Redo,
        69..=78 => Act::JobStep(r.below(3)),
        79..=83 => Act::JobsFinish,
        84..=86 => Act::Photo(r.below(8)),
        87..=88 => Act::Remove(r.below(2) == 0),
        89 => if r.below(3) == 0 { Act::SourceBroken(r.below(7)) } else { Act::SourceGone(r.below(7)) },
        90 => Act::SourceBack,
        91..=92 => Act::Export,
        93 => Act::ClearCache,
        94 => Act::Restart,
        95..=96 => Act::Develop(1 + r.below(40)),
        97 => if r.below(2) == 0 { Act::Kill } else { Act::NewPhoto },
        // (drawn like a wait, so that the sequences of earlier seeds stay what they were apart from this action;
        // `GRAINEO_HDR_STRESS_OLD=1`: exactly the earlier sequences)
        98 => match r.below(4) {
            0 if std::env::var_os("GRAINEO_HDR_STRESS_OLD").is_none() => Act::ViewBackup,
            k => Act::Wait([2, 7, 35, 70][k]),
        },
        _ => Act::Wait([2, 7, 35, 70][r.below(4)]),
    }
}

fn sequence(seed: u64, steps: usize) -> Vec<Act> {
    let mut r = Rng::new(seed);
    // every sequence starts with a merge (of at least two frames)
    let mut v = vec![Act::Create(3 + r.below(125))];
    v.extend((1..steps).map(|_| random_act(&mut r)));
    v
}

// ─────────────────────────────────────────────── the laboratory: one app, one bracket

static PANICS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FOLDER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// An export under way: what the recipe keys were when it started, and its document when it is done.
struct Exporting {
    keys: Vec<String>,
    /// the full result of the recipe was in the cache when the export started
    cached_at_start: bool,
    done: Arc<Mutex<Option<Result<(String, bool, hdr::Settings, Vec<PathBuf>), String>>>>,
}

struct Lab {
    ctx: egui::Context,
    a: App,
    time: f64,
    dir: PathBuf,
    /// the seven frames: (file, image, path), dark → bright
    frames: Vec<(i64, i64, PathBuf)>,
    /// file of the merged photo under test, as long as the library has it
    hdr: Option<i64>,
    /// path the merged photo is addressed by
    hdr_path: Option<PathBuf>,
    /// it was there once and is gone for good or taken out
    was: Option<i64>,
    /// file ids of every merged photo of this sequence
    ids: Vec<i64>,
    /// the job that creates it may take it out again (cancelled, undone, a source gone)
    may_vanish: bool,
    /// contrast set by hand on the merged photo (`None`: never)
    contrast: Option<f64>,
    /// what looking at a backup touched in the cache, until it is checked (it has to be nothing)
    viewed: Option<u64>,
    /// merged photos of this sequence for which a result was seen in the cache at some time
    results_seen: Vec<i64>,
    /// a result (file, key) whose computation the user cancelled and that is not in the cache: it is not computed
    /// again by itself (until it is asked for: "Neu berechnen", an export, a change)
    cancelled: Option<(i64, String)>,
    /// the recipe (file, key) as it was at rest when a source of it was found gone: it must not change before the
    /// source is back (nothing could be computed for the change)
    locked: Option<(i64, String)>,
    /// sources renamed away: (index of the frame)
    gone: Vec<usize>,
    /// sources overwritten with something that is no RAW
    broken: Vec<usize>,
    /// the last change of the recipe was cancelled or could not be computed: "Nicht aktuell" is the honest state
    interrupted: bool,
    exports: Vec<Exporting>,
    /// every key the stored recipe had, in order (for the exports)
    keys_seen: Vec<String>,
    trace: Vec<String>,
    /// the sequence left what the model follows (checked no further)
    over: bool,
    verbose: bool,
    jobs_before: usize,
    /// merged photos that were noted as unfinished when the app was quit while computing (checked after the restart)
    killed_unfinished: Vec<i64>,
    /// ordinary photos added while the sequence ran
    added: usize,
    /// image ids of every merged photo of this sequence
    image_ids: Vec<i64>,
    /// something was computed for the merged photo under test (as far as seen)
    had_result: bool,
    /// a ⌘Z of creating the merged photo waited for its first computation after the last step (it is carried out
    /// when that job ends, whatever the user does meanwhile)
    undo_waited: bool,
    /// a source was changed on disk at some time in this sequence
    ever_broken: bool,
}

type Check = Result<(), String>;

/// The three brackets of a batch sequence: (name, EV, second of the day).
fn batch_names() -> Vec<String> {
    (0..9).map(|i| format!("B{}_{}.dng", i / 3 + 1, i % 3 + 1)).collect()
}

/// The nine frames of the batch sequences once (three brackets of −2, 0, +2 EV, minutes apart).
fn batch_templates() -> &'static PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("graineo-hdr-stress-{}", std::process::id())).join("template-batch");
        std::fs::create_dir_all(&dir).unwrap();
        for (i, name) in batch_names().iter().enumerate() {
            let ev = (i % 3) as f64 * 2.0 - 2.0;
            tiny_dng(&dir.join(name), 2f64.powf(ev), 0.01 * 2f64.powf(ev), 100 + 300 * (i / 3) as u32 + (i % 3) as u32, 50 + i as u64);
        }
        dir
    })
}

fn frame_paths(dir: &Path) -> Vec<PathBuf> {
    (0..7).map(|i| dir.join(format!("S{:04}.dng", i + 1))).collect()
}

/// The seven frames once (the sequences copy them).
fn templates() -> &'static PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("graineo-hdr-stress-{}", std::process::id())).join("template");
        std::fs::create_dir_all(&dir).unwrap();
        for (i, p) in frame_paths(&dir).iter().enumerate() {
            let ev = i as f64 - 3.0;
            tiny_dng(p, 2f64.powf(ev), 0.01 * 2f64.powf(ev), 10 + i as u32, i as u64 + 1);
        }
        dir
    })
}

impl Lab {
    fn new(verbose: bool) -> Lab {
        let names: Vec<String> = frame_paths(Path::new("")).iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        Lab::open(verbose, templates(), &names)
    }

    /// A lab with copies of the frames `names` of the folder `from`.
    fn open(verbose: bool, from: &Path, names: &[String]) -> Lab {
        let n = FOLDER.fetch_add(1, Ordering::SeqCst);
        let dir = templates().parent().unwrap().join(format!("q{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        for name in names {
            std::fs::copy(from.join(name), dir.join(name)).unwrap();
        }
        let frames = {
            let db = Db::open(&library::db_path()).unwrap();
            library::reconcile::add_root(&db, &dir, &|_| {}).unwrap();
            names
                .iter()
                .map(|n| dir.join(n))
                .map(|p| {
                    let name = p.file_name().unwrap().to_string_lossy().to_string();
                    let (file, image): (i64, i64) = db
                        .conn
                        .query_row("SELECT d.id, b.id FROM file d JOIN image b ON b.file = d.id JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.name = ?1 AND s.path = ?2", (name, format!("{}/", dir.display())), |r| Ok((r.get(0)?, r.get(1)?)))
                        .unwrap();
                    (file, image, p)
                })
                .collect()
        };
        new_session();
        crate::hdr_ui::gate::reset();
        crate::hdr_ui::gate::set(true);
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut a = App::new(&cc);
        a.open_folder_inner(dir.clone());
        let mut lab = Lab { ctx, a, time: 10.0, dir, frames, hdr: None, hdr_path: None, was: None, ids: Vec::new(), may_vanish: false, contrast: None, gone: Vec::new(), broken: Vec::new(), interrupted: false, exports: Vec::new(), keys_seen: Vec::new(), trace: Vec::new(), over: false, verbose, jobs_before: 0, killed_unfinished: Vec::new(), added: 0, image_ids: Vec::new(), had_result: false, undo_waited: false, ever_broken: false, viewed: None, locked: None, cancelled: None, results_seen: Vec::new() };
        assert_eq!(lab.a.entries.len(), names.len(), "the frames in the library");
        lab.a.to_single_image();
        lab.settle();
        lab
    }

    fn frame(&mut self) {
        self.time += 0.1;
        let mut input = egui::RawInput { time: Some(self.time), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), ..Default::default() };
        self.a.raw_input_hook(&self.ctx, &mut input);
        let (a, ctx) = (&mut self.a, &self.ctx);
        let _ = crate::ui_test::run(ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        self.a.handle_results(&self.ctx);
    }

    /// Is anything still on its way that the test does not hold back (library writes, loads, helper threads, a
    /// document being opened or swapped in, cache files being written)?
    fn busy(&self) -> bool {
        self.a.lib.db_writer.busy()
            || self.a.lib.loader.busy()
            || !self.a.lib.loader.then.is_empty()
            || crate::hdr_ui::gate::busy() > 0
            || rawdev::merged::cache_busy()
            || self.a.loading.is_some()
            || crate::hdr_live::test_waiting() > 0
            || crate::hdr_auto::test_waiting()
            || self.hdr.is_some_and(|f| crate::hdr_paint::test_state(&self.ctx).0 == f && crate::hdr_paint::test_state(&self.ctx).3)
    }

    /// Frames until nothing moves any more (jobs waiting at a point stay there).
    fn settle(&mut self) {
        let t = std::time::Instant::now();
        let mut quiet = 0;
        // jobs that run but wait for a lock a stopped job holds never get further: no progress for a while counts as quiet
        let mut last = (crate::hdr_ui::gate::jobs(), rawdev::merged::generation());
        let mut since = std::time::Instant::now();
        while quiet < 3 {
            self.frame();
            if self.busy() {
                quiet = 0;
                let now = (crate::hdr_ui::gate::jobs(), rawdev::merged::generation());
                let only_jobs = !self.a.lib.db_writer.busy() && !self.a.lib.loader.busy() && self.a.loading.is_none() && !rawdev::merged::cache_busy() && crate::hdr_live::test_waiting() == 0;
                if now != last || !only_jobs {
                    last = now;
                    since = std::time::Instant::now();
                } else if since.elapsed() > std::time::Duration::from_millis(250) && !now.0.iter().all(|j| j.at.is_none()) {
                    // (only a job held at a point can hold what the others wait for; while every job runs, a long
                    // pause is the machine being busy – under load a thread can wait for its core longer than that)
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            } else {
                quiet += 1;
            }
            if t.elapsed() > std::time::Duration::from_secs(20) {
                self.note("settle: timeout".into());
                break;
            }
        }
    }

    fn note(&mut self, t: String) {
        if self.verbose {
            eprintln!("    {t}");
        }
        self.trace.push(t);
    }

    fn db(&self) -> Db {
        Db::open(&library::db_path()).unwrap()
    }

    /// The merged photos of this bracket's folder in the library.
    fn merged_files(&self) -> Vec<i64> {
        let db = self.db();
        let mut st = db.conn.prepare("SELECT m.file FROM merge m JOIN file d ON d.id = m.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE s.path = ?1 ORDER BY m.file").unwrap();
        let v: Vec<i64> = st.query_map([format!("{}/", self.dir.display())], |r| r.get(0)).unwrap().flatten().collect();
        v
    }

    fn recipe(&self) -> Option<(library::merge::Recipe, String)> {
        let db = self.db();
        let r = library::merge::recipe(&db.conn, self.hdr?)?;
        let key = r.cache_key(&db.conn);
        Some((r, key))
    }

    /// Index of the entry of the merged photo in the app's list.
    fn hdr_entry(&self) -> Option<usize> {
        let f = self.hdr?;
        self.a.entries.iter().position(|e| e.file == f)
    }

    fn shown_hdr(&self) -> bool {
        self.a.shown_merged().is_some_and(|(_, d)| d.merged.as_ref().is_some_and(|m| Some(m.source.file) == self.hdr))
    }

    fn click(&mut self, what: &str, arg: Option<usize>) {
        if !self.shown_hdr() {
            return;
        }
        self.a.tool_open = Some(crate::workspace::ToolTab::Merge);
        self.a.show_right = true;
        self.frame();
        crate::hdr_paint::test_click(what, arg);
        self.frame();
    }

    /// The question about own work of a merged photo, answered as the dialog does it.
    fn answer_ask(&mut self, yes: bool) {
        let Some(library_manage::Ask::RemoveMerge { files, undo, .. }) = self.a.lib.manage.ask.take() else { return };
        self.note(format!("question about own work: {}", if yes { "HDR entfernen" } else { "Abbrechen" }));
        if !yes {
            return;
        }
        if undo {
            self.a.lib.confirm_merge_undo(files);
        } else {
            self.a.lib.remove_merge_now(files);
        }
    }

    fn act(&mut self, act: Act) {
        use crate::hdr_ui::gate;
        match act {
            Act::Create(mask) => {
                if self.hdr.is_some() || !self.gone.is_empty() || !self.broken.is_empty() {
                    return;
                }
                let mut pick: Vec<i64> = self.frames.iter().enumerate().filter(|(i, _)| mask >> i & 1 == 1).map(|(_, f)| f.1).collect();
                if pick.len() < 2 {
                    pick = self.frames[2..5].iter().map(|f| f.1).collect();
                }
                self.a.to_grid();
                self.a.lib.selection = pick.iter().copied().collect();
                self.a.lib.active = Some(pick[0]);
                self.a.lib.nothing_selected = false;
                crate::hdr_ui::clock::advance(std::time::Duration::from_secs(1));
                self.a.open_hdr();
                self.frame();
                if let Some(q) = crate::hdr_ui::test_question() {
                    self.note(format!("question: {q}"));
                    crate::hdr_ui::test_new_question();
                }
            }
            Act::Tick(row) => {
                let Some(info) = self.a.merge_info() else { return };
                // rows: the frames of the merge first, then the photos offered
                let n = info.sources.len();
                let offered: Vec<i64> = self.frames.iter().map(|f| f.0).filter(|f| !info.source_files.contains(f)).collect();
                let row = row % (n + offered.len()).max(1);
                if row < n {
                    self.click("leave", Some(row));
                } else {
                    self.click("drop", Some(offered[row - n] as usize));
                }
            }
            Act::Reference(i) => {
                let n = self.a.merge_info().map_or(1, |i| i.sources.len());
                self.click("reference", Some(i % n.max(1)));
            }
            Act::Ghost(k) => self.click("ghost", Some(k)),
            Act::Output(k) => self.click("output", Some(k)),
            Act::Align => self.click("align", None),
            Act::Strength(v) => self.click("strength", Some(v)),
            Act::Paint(seed) => self.click("paint", Some(seed)),
            Act::AreaFrame(i, k) => {
                let n = self.a.merge_info().map_or(1, |i| i.sources.len());
                self.click("area_frame", Some(i * 16 + k % n.max(1)));
            }
            Act::Eye(i) => self.click("eye", Some(i)),
            Act::DeleteArea(i) => self.click("delete", Some(i)),
            Act::Cancel => {
                if let Some(f) = self.hdr.filter(|_| self.shown_hdr()) {
                    let running = rawdev::merged::jobs().iter().any(|j| j.file == f) || crate::hdr_ui::job_alive(f);
                    if running {
                        if crate::hdr_ui::creating(f) {
                            self.may_vanish = true;
                        }
                        self.interrupted = true;
                    }
                }
                self.click("cancel", None);
            }
            Act::Recompute => self.click("recompute", None),
            Act::Restore => self.click("restore", None),
            Act::Undo => {
                if let Some(f) = self.hdr {
                    if crate::hdr_ui::creating(f) {
                        self.may_vanish = true;
                    }
                }
                self.a.undo_latest();
                self.frame();
                let yes = self.a.lib.manage.ask.is_some();
                self.answer_ask(yes);
            }
            Act::Redo => self.a.redo_latest(),
            Act::JobStep(k) => {
                let waiting: Vec<gate::Job> = gate::jobs().into_iter().filter(|j| j.at.is_some()).collect();
                if let Some(j) = waiting.get(k % waiting.len().max(1)) {
                    self.note(format!("job {} of {} ({}) passes {:?}", j.id, j.file, if j.update { "change" } else { "create" }, j.at));
                    gate::step(j.id);
                }
            }
            Act::JobsFinish => self.finish_jobs(),
            Act::Photo(k) => {
                self.a.to_single_image();
                let target = if k >= 7 { self.hdr_entry() } else { self.a.entries.iter().position(|e| e.file == self.frames[k].0) };
                match target {
                    Some(i) => {
                        self.a.lib.set_active(self.a.entries[i].image);
                        self.a.select(i, false);
                    }
                    // (a frame inside the collapsed stack is not in the list: the grid instead)
                    None => self.a.to_grid(),
                }
            }
            Act::Remove(yes) => {
                let Some(f) = self.hdr else { return };
                let mut actions = Vec::new();
                self.a.lib.remove_merge(f, &mut actions);
                self.a.library_actions(actions);
                self.answer_ask(yes);
            }
            Act::SourceGone(k) => {
                let k = k % 7;
                if self.gone.contains(&k) {
                    return;
                }
                let p = &self.frames[k].2;
                if std::fs::rename(p, p.with_extension("away")).is_ok() {
                    self.gone.push(k);
                    if self.hdr.is_some_and(|f| !gate::jobs().iter().filter(|j| j.file == f).collect::<Vec<_>>().is_empty()) {
                        // a job that reads the frames now fails: creating → the photo goes, a change → out of date
                        self.may_vanish = self.hdr.is_some_and(crate::hdr_ui::creating);
                        self.interrupted = true;
                    }
                }
            }
            Act::SourceBack => {
                for k in std::mem::take(&mut self.gone) {
                    let p = &self.frames[k].2;
                    let _ = std::fs::rename(p.with_extension("away"), p);
                }
                for k in std::mem::take(&mut self.broken) {
                    let _ = std::fs::copy(&frame_paths(templates())[k], &self.frames[k].2);
                }
            }
            Act::SourceBroken(k) => {
                let k = k % 7;
                if !self.gone.contains(&k) && !self.broken.contains(&k) && std::fs::write(&self.frames[k].2, b"not a raw file").is_ok() {
                    self.broken.push(k);
                    self.ever_broken = true;
                    if self.hdr.is_some_and(crate::hdr_ui::job_alive) {
                        self.may_vanish = self.hdr.is_some_and(crate::hdr_ui::creating);
                    }
                    self.interrupted = true;
                }
            }
            Act::Export => {
                let (Some(path), Some(_)) = (self.hdr_path.clone(), self.hdr) else { return };
                let Some((_, key)) = self.recipe() else { return };
                let done = Arc::new(Mutex::new(None));
                let out = done.clone();
                let settings = self.hdr_entry().map(|i| self.a.entries[i].settings.clone()).unwrap_or_default();
                crate::hdr_ui::spawn(move || {
                    let r = rawdev::RawDoc::open_for_output(&path, &settings, &|| true).and_then(|d| d.ok_or_else(|| "abgebrochen".to_string())).and_then(|d| {
                        let m = d.merged.clone().ok_or_else(|| "not a merged photo".to_string())?;
                        Ok((m.source.key.clone(), m.pending, m.source.settings.clone(), m.source.sources.clone()))
                    });
                    *out.lock().unwrap() = Some(r);
                });
                let cached_at_start = rawdev::merged::cached(&key) && rawdev::merged::camera_cached(&key);
                self.exports.push(Exporting { keys: vec![key], cached_at_start, done });
            }
            Act::ClearCache => {
                let freed = rawdev::merged::clear_cache();
                self.note(format!("cache cleared: {freed} bytes"));
            }
            Act::Restart => {
                // a quiet app is ended ([`Act::Kill`] ends one that computes)
                self.finish_jobs();
                self.before_the_end();
                new_session();
                gate::reset();
                gate::set(true);
                self.ctx = egui::Context::default();
                let cc = eframe::CreationContext::_new_kittest(self.ctx.clone());
                self.a = App::new(&cc);
                self.a.open_folder_inner(self.dir.clone());
                self.a.to_single_image();
                self.settle();
                if let Some(i) = self.hdr_entry() {
                    self.a.lib.set_active(self.a.entries[i].image);
                    self.a.select(i, false);
                }
            }
            Act::Develop(v) => {
                if !self.shown_hdr() {
                    return;
                }
                if let Some(i) = self.hdr_entry() {
                    let c = v as f64 / 100.0;
                    self.a.entries[i].settings.contrast = c;
                    self.contrast = Some(c);
                    // (as the app does when a slider is let go: on its way to the library, not written yet)
                    self.a.save_current();
                }
            }
            Act::Wait(tenths) => crate::hdr_ui::clock::advance(std::time::Duration::from_millis(100 * tenths as u64)),
            Act::Kill => {
                self.before_the_end();
                self.interrupted = true;
                self.kill();
                if let Some(i) = self.hdr_entry() {
                    self.a.lib.set_active(self.a.entries[i].image);
                    self.a.select(i, false);
                }
            }
            Act::ViewBackup => {
                // (the process that looks at a backup knows that its library is not the real one; the library itself
                // is not swapped here: threads of the app read it meanwhile)
                rawdev::merged::test_other_library(true);
                let mut freed = rawdev::merged::sweep() + rawdev::merged::clear_cache() + rawdev::merged::trimmable().len() as u64;
                for f in self.ids.clone() {
                    freed += rawdev::merged::tidy(f, true);
                }
                rawdev::merged::test_other_library(false);
                self.note(format!("a backup was looked at: {freed} bytes or files of the cache touched"));
                self.viewed = Some(freed);
            }
            Act::NewPhoto => {
                self.new_photo();
                // The sync also reads the sources: one that was changed on disk gets its new size and time, and the
                // key of the merged photo follows. A merged photo open meanwhile goes on showing its old result as
                // "Aktuell" until it is opened again – known, and beyond what this model follows.
                if self.ever_broken && self.hdr.is_some() {
                    self.note("a changed source was synced while its merged photo may be open: the sequence ends here".into());
                    self.over = true;
                }
            }
        }
    }

    /// The app is about to end: what it holds goes to the library (as on quitting), and the model notes what the
    /// next start may do – a merged photo still noted as unfinished, with nothing computed for it and no work on it,
    /// is removed then; every other one stays.
    fn before_the_end(&mut self) {
        self.a.save_current();
        library::store::flush(std::time::Duration::from_secs(10));
        let t = std::time::Instant::now();
        while self.a.lib.db_writer.busy() && t.elapsed() < std::time::Duration::from_secs(10) {
            self.frame();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.killed_unfinished = library::merge::unfinished(&self.db().conn).iter().map(|u| u.file).collect();
        if let Some(f) = self.hdr {
            // (cache files of an older state of the recipe may be tidied away before the start looks: only a result
            // of the stored recipe, a grid preview or work keep the photo for sure)
            let safe = self.has_result(f) || library::preview::has_any_developed(f) || self.has_work(f);
            self.may_vanish |= self.killed_unfinished.contains(&f) && !safe;
        }
    }

    /// Does the merged photo `file` have a result that can be shown (the preview of its stored recipe is in the cache)?
    fn has_result(&self, file: i64) -> bool {
        let db = self.db();
        library::merge::recipe(&db.conn, file).is_some_and(|r| rawdev::merged::preview_cached(&r.cache_key(&db.conn)))
    }

    fn has_work(&self, file: i64) -> bool {
        super::has_own_work(&self.db(), file)
    }

    /// The app is quit while whatever runs is running (as a process that ends: the develop settings are saved, the
    /// jobs stop and nothing of their end reaches the library), and started again: the checks of the start run.
    fn kill(&mut self) {
        use crate::hdr_ui::gate;
        self.before_the_end();
        crate::hdr_ui::test_end_session();
        gate::set(false);
        let t = std::time::Instant::now();
        while (!gate::jobs().is_empty() || gate::busy() > 0 || !rawdev::merged::jobs().is_empty() || rawdev::merged::cache_busy()) && t.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if t.elapsed() >= std::time::Duration::from_secs(20) {
            self.note("quit: jobs did not end".into());
        }
        new_session();
        gate::reset();
        gate::set(true);
        self.ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(self.ctx.clone());
        self.a = App::new(&cc);
        self.a.open_folder_inner(self.dir.clone());
        self.a.to_single_image();
        self.settle();
        // (the check of the start runs with the first frames; its removal reloads the rows)
        for _ in 0..3 {
            self.frame();
        }
        self.settle();
    }

    /// An ordinary photo comes into the library: it never takes the file or image id a merged photo had (a removed
    /// one can come back with ⌘Z; previews and cache files hang on the id), and it starts without a developed preview.
    fn new_photo(&mut self) {
        self.added += 1;
        let path = self.dir.join(format!("N{:03}.dng", self.added));
        tiny_dng(&path, 1.0, 0.01, 40_000 + self.added as u32 * 60, 900 + self.added as u64);
        let db = self.db();
        let _ = library::reconcile::reconcile(&db, None, &|_| {});
        drop(db);
        self.a.lib.reload();
        self.settle();
    }

    /// The check of [`Self::new_photo`] (after the step).
    fn check_new_photos(&self) -> Check {
        let db = self.db();
        for k in 1..=self.added {
            let name = format!("N{k:03}.dng");
            let row: Option<(i64, i64)> = db.conn.query_row("SELECT d.id, b.id FROM file d JOIN image b ON b.file = d.id JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.name = ?1 AND s.path = ?2", (name.clone(), format!("{}/", self.dir.display())), |r| Ok((r.get(0)?, r.get(1)?))).ok();
            let Some((file, image)) = row else { return Err(format!("the new photo {name} is not in the library")) };
            if self.ids.contains(&file) {
                return Err(format!("the new photo {name} got the file id {file} of a merged photo ({:?})", self.ids));
            }
            if self.image_ids.contains(&image) {
                return Err(format!("the new photo {name} got the image id {image} of a merged photo ({:?})", self.image_ids));
            }
            if library::merge::is_virtual(&db.conn, file) {
                return Err(format!("the new photo {name} counts as a merged photo"));
            }
        }
        Ok(())
    }

    /// Every job runs to its end; afterwards jobs stop at the points again.
    fn finish_jobs(&mut self) {
        use crate::hdr_ui::gate;
        gate::set(false);
        let t = std::time::Instant::now();
        loop {
            self.settle();
            if gate::jobs().is_empty() && rawdev::merged::jobs().is_empty() && !self.busy() {
                break;
            }
            if t.elapsed() > std::time::Duration::from_secs(30) {
                self.note("jobs do not end".into());
                break;
            }
        }
        gate::set(true);
    }

    /// One step: the action, everything that is not held back settles, the model follows, the checks.
    fn step(&mut self, n: usize, act: Act) -> Check {
        self.jobs_before = crate::hdr_ui::gate::started();
        if self.verbose {
            eprintln!("{n:3} {act:?}");
        }
        self.trace.push(format!("{n:3} {act:?}"));
        // jobs told to stop do not wait for ever: with more than three waiting the oldest runs on
        let waiting = crate::hdr_ui::gate::jobs();
        if waiting.len() > 3 {
            for j in &waiting[..waiting.len() - 3] {
                self.note(format!("job {} runs to its end", j.id));
                crate::hdr_ui::gate::finish(j.id);
            }
            self.settle();
            // (what the end of these jobs did to the library – a ⌘Z that waited for one of them is carried out now – is
            // followed before the action: seed 31400241, a second ⌘Z in the same step brought the photo before back)
            self.follow(Act::JobsFinish)?;
            if self.over {
                return Ok(());
            }
        }
        // a question on screen is modal: nothing else can be done before it is answered (here: "Abbrechen"; ⌘Z and
        // "HDR entfernen" answer the question they ask themselves)
        if self.a.lib.manage.ask.is_some() {
            self.answer_ask(false);
            self.settle();
        }
        self.act(act);
        self.settle();
        if self.verbose {
            let steps: Vec<String> = self
                .a
                .lib
                .manage
                .undo
                .iter()
                .map(|u| match u {
                    library_manage::Undo::Merge { file, .. } => format!("Merge({file})"),
                    library_manage::Undo::MergeRemoved { files, .. } => format!("MergeRemoved({files:?})"),
                    library_manage::Undo::MergeBatch { .. } => "MergeBatch".into(),
                    _ => "other".into(),
                })
                .collect();
            eprintln!("      ⌘Z steps {steps:?}, waiting {:?}, question {}, in the library {:?}", self.a.lib.manage.merge_undo_waiting, self.a.lib.manage.ask.is_some(), self.merged_files());
        }
        self.follow(act)?;
        if self.over {
            return Ok(());
        }
        self.check(act)
    }

    /// The model follows what the library holds now; a merged photo appearing or vanishing has to have a reason.
    fn follow(&mut self, act: Act) -> Check {
        let now = self.merged_files();
        if now.len() > 1 {
            // ⌘Z brought an earlier merged photo back beside the newer one: two photos are beyond this model
            self.note(format!("two merged photos ({now:?}): the sequence ends here"));
            self.over = true;
            return Ok(());
        }
        match (self.hdr, now.first().copied()) {
            (None, Some(f)) => {
                if !matches!(act, Act::Create(_) | Act::Undo | Act::Redo) {
                    return Err(format!("a merged photo ({f}) appeared after {act:?}"));
                }
                if matches!(act, Act::Undo) && !self.ids.contains(&f) {
                    return Err(format!("⌘Z brought back photo {f}, removed were {:?}", self.ids));
                }
                // a new merged photo never has the file id of an earlier one (cache, previews and session state hang on it)
                if matches!(act, Act::Create(_)) && self.ids.iter().any(|old| f <= *old) {
                    return Err(format!("the new merged photo got the file id {f}; earlier ones had {:?}", self.ids));
                }
                if !self.ids.contains(&f) {
                    self.ids.push(f);
                }
                if let Ok(image) = self.db().conn.query_row("SELECT id FROM image WHERE file = ?1 AND copy IS NULL", [f], |r| r.get::<_, i64>(0)) {
                    if !self.image_ids.contains(&image) {
                        self.image_ids.push(image);
                    }
                }
                self.hdr = Some(f);
                self.had_result = rawdev::merged::has_cached(f) || library::preview::has_any_developed(f);
                self.may_vanish = false;
                self.interrupted = false;
                let db = self.db();
                self.hdr_path = library::merge::file_path(&db.conn, f);
                if matches!(act, Act::Create(_)) {
                    self.contrast = None;
                    self.keys_seen.clear();
                }
                self.note(format!("merged photo {f} is in the library"));
            }
            (Some(f), None) => {
                let allowed = matches!(act, Act::Undo | Act::Remove(_)) || (self.may_vanish && matches!(act, Act::JobStep(_) | Act::JobsFinish | Act::Cancel | Act::Restart | Act::SourceGone(_) | Act::Wait(_)))
                    // any action lets the app take over the end of a job that was told to stop
                    || self.may_vanish
                    || self.undo_waited;
                if !allowed {
                    return Err(format!("the merged photo {f} vanished after {act:?} (nobody removed it)"));
                }
                self.hdr = None;
                self.was = Some(f);
                self.may_vanish = false;
                self.note(format!("merged photo {f} left the library"));
            }
            (Some(f), Some(g)) if f != g => return Err(format!("merged photo {f} became {g}")),
            _ => {}
        }
        if matches!(act, Act::Kill | Act::Restart) {
            // what the session left unfinished is settled at the start: removed, or it has something to show
            self.may_vanish = false;
            let db = self.db();
            let left = library::merge::unfinished(&db.conn);
            if !left.is_empty() {
                return Err(format!("after the restart merged photos still count as unfinished: {:?}", left.iter().map(|u| u.file).collect::<Vec<_>>()));
            }
            for f in std::mem::take(&mut self.killed_unfinished) {
                if library::merge::is_virtual(&db.conn, f) && !(rawdev::merged::has_cached(f) || library::preview::has_any_developed(f) || super::has_own_work(&db, f)) {
                    return Err(format!("the unfinished merged photo {f} was left in the library with nothing to show"));
                }
            }
        }
        self.undo_waited = self.hdr.is_some_and(|f| self.a.lib.manage.merge_undo_waiting.contains(&f));
        self.check_new_photos()?;
        if let Some((_, key)) = self.recipe() {
            if self.keys_seen.last() != Some(&key) {
                self.keys_seen.push(key.clone());
            }
            for e in &mut self.exports {
                if e.done.lock().unwrap().is_none() && e.keys.last() != Some(&key) {
                    e.keys.push(key.clone());
                }
            }
        }
        Ok(())
    }

    /// The checks after every step.
    fn check(&mut self, act: Act) -> Check {
        use crate::hdr_ui::gate;
        if PANICS.load(Ordering::SeqCst) > 0 {
            return Err("a thread panicked".into());
        }
        if let Some(freed) = self.viewed.take() {
            if freed > 0 {
                return Err(format!("looking at a backup removed or offered {freed} bytes or files of the merge cache"));
            }
        }
        // no job storm: an action starts a job or two, never a series
        let started = gate::started().saturating_sub(self.jobs_before);
        if started > 4 {
            return Err(format!("{started} jobs started by one step ({act:?})"));
        }
        if gate::jobs().len() > 6 {
            return Err(format!("{} jobs alive", gate::jobs().len()));
        }
        // one merge at a time, whoever asks for it (jobs, exports, zooming in)
        let computing = rawdev::merged::jobs();
        if computing.len() > 1 {
            return Err(format!("{} merges are computed at once: {:?}", computing.len(), computing.iter().map(|j| j.file).collect::<Vec<_>>()));
        }
        // exports that are done: never the reference standing in, always a state the recipe had while it ran
        let mut i = 0;
        while i < self.exports.len() {
            let r = self.exports[i].done.lock().unwrap().take();
            match r {
                None => i += 1,
                Some(r) => {
                    let e = self.exports.remove(i);
                    match r {
                        Ok((key, pending, _, _)) => {
                            if pending {
                                return Err("an export got the reference frame instead of the merge".into());
                            }
                            if !e.keys.contains(&key) {
                                return Err(format!("an export got the result {key}, the recipe had {:?} meanwhile", e.keys));
                            }
                        }
                        // an honest error (a source is gone and the result is not in the cache, the photo was removed)
                        Err(why) => {
                            // the recipe is the one the export started with, its full result is in the cache, the
                            // photo is there: nothing can excuse a failure – also not a source that is gone
                            let stored = self.recipe().map(|r| r.1);
                            let quiet = crate::hdr_ui::gate::jobs().is_empty() && rawdev::merged::jobs().is_empty() && !rawdev::merged::cache_busy();
                            if quiet && self.hdr.is_some() && e.keys.len() == 1 && stored.as_ref() == e.keys.first() && e.cached_at_start && stored.as_ref().is_some_and(|k| rawdev::merged::cached(k)) && rawdev::merged::camera_cached(&e.keys[0]) {
                                return Err(format!("an export failed although its result is in the cache: {why}"));
                            }
                            self.note(format!("export: {why}"))
                        }
                    }
                }
            }
        }
        // (also while a merged photo is taken out: a job of it may still end and leave its result and its tile, the
        // cache may be emptied, and ⌘Z brings the photo back with the tile of that result – seed 96200251)
        for f in self.ids.clone() {
            if !self.results_seen.contains(&f) && rawdev::merged::has_cached(f) {
                self.results_seen.push(f);
            }
        }
        let Some(file) = self.hdr else { return Ok(()) };
        self.had_result |= self.results_seen.contains(&file);
        let Some((recipe, key)) = self.recipe() else { return Err(format!("merged photo {file} has no recipe")) };
        // a merged photo nothing was computed for has no picture of its own: its reference frame, which stands in on
        // screen, is never stored as its grid preview
        self.had_result |= rawdev::merged::has_cached(file) || rawdev::merged::jobs().iter().any(|j| j.file == file) || gate::jobs().iter().any(|j| j.file == file && j.at != Some(gate::Point::Start));
        // (the tile of a job arrives beside the job: it may be stored a moment before the job's files are – seed
        // 96200251, where the picture stored was the one of the job's own result)
        if !self.had_result && library::preview::has_any_developed(file) {
            let t = std::time::Instant::now();
            while !rawdev::merged::has_cached(file) && t.elapsed() < std::time::Duration::from_secs(1) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            self.had_result |= rawdev::merged::has_cached(file);
        }
        if !self.had_result && library::preview::has_any_developed(file) {
            return Err(format!("a grid preview was stored for the merged photo {file} although nothing was computed for it (its reference frame)"));
        }
        // "Abbrechen" holds: a result the user stopped is not computed again unasked (QA window run 9, M2: the full
        // resolution was merged "ahead" two seconds after the cancel)
        {
            if matches!(act, Act::Export | Act::Recompute | Act::Create(_) | Act::Kill | Act::Restart | Act::ClearCache) || self.cancelled.as_ref().is_some_and(|c| c.0 != file || c.1 != key) {
                self.cancelled = None;
            }
            let quiet = gate::jobs().is_empty() && rawdev::merged::jobs().is_empty() && !self.busy() && crate::hdr_ui::turn::queue().is_empty();
            match &self.cancelled {
                // (the result itself in the cache – a job of the photo alone says nothing: it may compute a comparison
                // the user asked for meanwhile, another key; seed 5530132)
                Some(_) if rawdev::merged::cached(&key) => {
                    return Err(format!("the result {key} was cancelled by the user and is computed again without being asked for ({act:?})"));
                }
                // (cancelled, not failed: a result that failed for a missing source is tried again when the source is back)
                None if quiet && self.exports.is_empty() && crate::hdr_ui::stopped(file, &key) && crate::hdr_ui::failed_reason(file, &key).is_none() && !rawdev::merged::cached(&key) => self.cancelled = Some((file, key.clone())),
                _ => {}
            }
        }
        // a source of the recipe is gone: from the moment everything is at rest the recipe stays as it is
        {
            let source_gone = recipe.sources.iter().any(|s| self.frames.iter().position(|f| f.0 == s.file).is_some_and(|k| self.gone.contains(&k)));
            let quiet = gate::jobs().is_empty() && rawdev::merged::jobs().is_empty() && !self.busy() && crate::hdr_ui::turn::queue().is_empty();
            match &self.locked {
                _ if !source_gone => self.locked = None,
                Some((f, k)) if *f == file && *k != key => return Err(format!("the recipe changed from {k} to {key} while a source of it is gone ({act:?})")),
                Some((f, _)) if *f == file => {}
                _ if quiet => self.locked = Some((file, key.clone())),
                _ => {}
            }
        }
        // the recipe itself
        let settings = rawdev::merged::parse_settings(&recipe.settings).map_err(|e| format!("recipe not readable: {e}"))?;
        let n = recipe.sources.len();
        if n < 2 {
            return Err(format!("recipe with {n} sources"));
        }
        let mut ids: Vec<i64> = recipe.sources.iter().map(|s| s.file).collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() != n || ids.iter().any(|f| !self.frames.iter().any(|x| x.0 == *f)) {
            return Err(format!("recipe sources {:?} are not {n} frames of the bracket", recipe.sources.iter().map(|s| s.file).collect::<Vec<_>>()));
        }
        match settings.reference {
            Some(r) if r < n => {}
            r => return Err(format!("reference {r:?} of {n} frames")),
        }
        if let Some(a) = settings.regions.iter().find(|a| a.frame >= n) {
            return Err(format!("an area takes frame {} of {n}", a.frame));
        }
        if self.verbose {
            let stored = library::merge::work(&self.db().conn, file).develop.first().and_then(|j| serde_json::from_str::<crate::Settings>(j).ok()).map(|s| s.contrast);
            eprintln!("      develop: model {:?}, stored {stored:?}, entry {:?}", self.contrast, self.hdr_entry().map(|i| (self.a.entries[i].loaded, self.a.entries[i].settings.contrast)));
        }
        // (⌘Z and ⇧⌘Z move the develop history: the slider follows them)
        if matches!(act, Act::Undo | Act::Redo) && self.contrast.is_some() {
            self.contrast = self.hdr_entry().filter(|i| self.a.entries[*i].loaded).map(|i| self.a.entries[i].settings.contrast).or(self.contrast);
        }
        // the develop settings of the photo are never lost
        if let Some(c) = self.contrast {
            let shown = self.hdr_entry().filter(|i| self.a.entries[*i].loaded).map(|i| self.a.entries[i].settings.contrast);
            if shown.is_some_and(|s| (s - c).abs() > 1e-9) {
                return Err(format!("contrast set by hand {c} became {shown:?}"));
            }
        }
        // the cache does not grow without bound: the stored state and a few states of this session
        let (results, bases) = cache_keys(file);
        if results.len() > 6 || bases.len() > 5 {
            return Err(format!("cache of the photo holds {} results and {} bases", results.len(), bases.len()));
        }
        // the tool and the photo shown
        if !self.shown_hdr() {
            return Ok(());
        }
        let Some(info) = self.a.merge_info() else { return Err("the merged photo is shown, the tool does not know it".into()) };
        let (_, doc) = self.a.shown_merged().unwrap();
        let m = doc.merged.clone().unwrap();
        if info.sources.len() != m.source.sources.len() {
            return Err("list of the tool and photo shown differ".into());
        }
        // the rows are the frames of the picture shown, looked up by their own files (QA 11, M-1: while a change of the
        // frames was computed the list took the recipe's files and showed a row too many with shifted EVs)
        if info.list_files.len() != info.sources.len() || info.source_exif_ev.len() != info.sources.len() {
            return Err(format!("rows of the list: {} files, {} EVs for {} frames shown", info.list_files.len(), info.source_exif_ev.len(), info.sources.len()));
        }
        if !info.in_step {
            let db = self.db();
            let paths: Vec<PathBuf> = info.list_files.iter().filter_map(|f| library::merge::file_path(&db.conn, *f)).collect();
            if paths != info.sources {
                return Err(format!("while the lists differ the rows name other files than the picture's frames: {:?}", info.list_files));
            }
        }
        if info.in_step {
            let db = self.db();
            let paths: Vec<PathBuf> = info.source_files.iter().filter_map(|f| library::merge::file_path(&db.conn, *f)).collect();
            if paths != info.sources || info.source_files != recipe.sources.iter().map(|s| s.file).collect::<Vec<_>>() {
                return Err(format!("the list can be changed, but it is not the recipe's: list {:?}, recipe {:?}", info.source_files, recipe.sources.iter().map(|s| s.file).collect::<Vec<_>>()));
            }
        }
        let (line, cancel) = crate::hdr_paint::test_status(&info);
        if self.verbose {
            eprintln!("      [{line}] pending {} stale {} busy {} in_step {} failed {:?} missing {:?} wanted {:?} shown {} stored {key} jobs {:?}", info.pending, info.stale, info.busy, info.in_step, info.failed, info.missing, crate::hdr_ui::wanted(file).map(|w| w.key), m.source.key, gate::jobs().iter().map(|j| (j.id, j.at)).collect::<Vec<_>>());
        }
        let (tool_file, tool, hidden, _) = crate::hdr_paint::test_state(&self.ctx);
        let wanted = crate::hdr_ui::wanted(file).map(|w| w.key);
        let current = line.starts_with("Aktuell") || line.starts_with("Vorschau aktuell · volle Auflösung beim Zoomen");
        if current && !hidden && m.source.key != key {
            return Err(format!("status \"{line}\", but the photo shows {} and the recipe is {key} (wanted {wanted:?})", m.source.key));
        }
        // "Aktuell · volle Auflösung" says that the full result is there. ("Vorschau aktuell · volle Auflösung beim
        // Zoomen" promises nothing of the cache: the document on screen may be older than the cache – the
        // recipe left its state, the state was tidied away, ⌘Z brought the recipe back to it (seed 512287) – and the
        // full resolution is merged when it is asked for.)
        if line.starts_with("Aktuell") && !hidden && !self.a.in_grid() && !rawdev::merged::cached(&m.source.key) && !matches!(act, Act::ClearCache) {
            // (after "Leeren" the photo on screen is still right; it is computed again when opened)
            let cleared = self.trace.iter().rev().take(60).any(|t| t.contains("ClearCache"));
            if !cleared {
                return Err(format!("status \"{line}\", but the cache has no result {}", m.source.key));
            }
        }
        // at rest: nothing runs, nothing waits – the states agree and nothing is locked without a way out
        // (a merge an output asked for may wait for its turn or compute beside the jobs of the test: not at rest)
        let rest = gate::jobs().is_empty() && rawdev::merged::jobs().is_empty() && !self.busy() && crate::hdr_ui::turn::queue().is_empty();
        if !rest {
            return Ok(());
        }
        if crate::hdr_ui::job_alive(file) || !self.a.lib.manage.merge_undo_waiting.is_empty() {
            return Err(format!("no job runs, but something still waits: job alive {}, ⌘Z waiting {:?}", crate::hdr_ui::job_alive(file), self.a.lib.manage.merge_undo_waiting));
        }
        if crate::hdr_ui::creating(file) {
            return Err("no job runs, the photo still counts as being created".into());
        }
        let missing = !m.source.missing().is_empty() || !self.gone.is_empty() || !self.broken.is_empty();
        if cancel {
            return Err(format!("no job runs, the tool offers \"Abbrechen\" (\"{line}\")"));
        }
        // a photo that does not show its result says why, and a locked list has a way out
        if (info.pending || info.stale || info.failed.is_some()) && !(line.starts_with("Nicht aktuell") || line.starts_with("Quelle fehlt") || line.starts_with("Vorschau aktuell · volle Auflösung abgebrochen")) && matches!(act, Act::Wait(t) if t >= 35) {
            return Err(format!("the photo does not show its result (pending {}, out of date {}, failed {:?}), the status says \"{line}\"", info.pending, info.stale, info.failed));
        }
        if !info.in_step && !(info.stale || info.failed.is_some()) && matches!(act, Act::Wait(t) if t >= 70) {
            return Err(format!("at rest the list of frames is locked without a way out (\"{line}\")"));
        }
        if info.stale || info.failed.is_some() || missing || self.interrupted {
            // out of date with a way out ("Neu berechnen"), or a source is gone: honest states, checked no further
            if !info.stale && !missing && m.source.key == key && !info.pending {
                self.interrupted = false;
            }
            return Ok(());
        }
        // (the short memories of the tool: half a second for the recipe, three seconds "a change went out just now")
        if !matches!(act, Act::Wait(t) if t >= 35) {
            return Ok(());
        }
        if info.pending {
            return Err(format!("at rest the photo still shows its reference (\"{line}\"), no job runs, the sources are there"));
        }
        if !current {
            return Err(format!("at rest the status is \"{line}\""));
        }
        if !info.in_step {
            return Err("at rest the list of frames is locked".into());
        }
        if tool_file == file && !hidden {
            let pixels = |x: &hdr::Settings| hdr::Settings { auto_tone: None, ..x.clone() };
            if pixels(&tool) != pixels(&settings) {
                return Err(format!("at rest the tool holds other settings than the recipe:\n  tool   {}\n  recipe {}", serde_json::to_string(&tool).unwrap_or_default(), recipe.settings));
            }
        }
        if !hidden && wanted.as_ref().is_some_and(|w| *w != key) {
            return Err(format!("at rest another result is wanted ({wanted:?}) than the recipe's ({key})"));
        }
        Ok(())
    }

    /// The end of a sequence: everything runs out, time passes, the photo is left – then the stored state is all that
    /// is left in the cache, and its pixels are the ones a fresh merge of the recipe gives.
    fn finish(&mut self) -> Check {
        self.act(Act::SourceBack);
        self.step(900, Act::JobsFinish)?;
        self.step(901, Act::Photo(7))?;
        self.step(902, Act::Wait(70))?;
        self.step(903, Act::JobsFinish)?;
        self.step(904, Act::Wait(70))?;
        if !self.exports.is_empty() {
            return Err("an export never ended".into());
        }
        let Some(file) = self.hdr else { return Ok(()) };
        let Some((recipe, key)) = self.recipe() else { return Ok(()) };
        // the pixels of the photo shown and of the cache against a fresh merge of the stored recipe
        let settings = rawdev::merged::parse_settings(&recipe.settings).map_err(|e| e.to_string())?;
        let sources: Vec<PathBuf> = recipe.sources.iter().map(|s| self.frames.iter().find(|f| f.0 == s.file).unwrap().2.clone()).collect();
        let fresh = rawdev::merged::MergedSource::new(900_000_000 + file, sources, settings, format!("f{}-{:016x}", 900_000_000 + file, 1), format!("f{}-{:016x}", 900_000_000 + file, 2));
        let cancel = AtomicBool::new(false);
        rawdev::merged::compute(&fresh, &cancel, &|_, _| {}, &mut |_, _| {})?;
        let want = rawdev::merged::read_preview(&fresh.key).ok_or("fresh merge not readable")?;
        let want_full = rawdev::merged::read_full(&fresh.key).ok_or("fresh merge not readable")?;
        while rawdev::merged::cache_busy() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        rawdev::merged::forget_file(fresh.file);
        let differs = |a: &[[f32; 3]], b: &[[f32; 3]]| a.len() != b.len() || pixels_differ(a, b);
        if self.verbose {
            let maxd = |a: &[[f32; 3]], b: &[[f32; 3]]| a.iter().zip(b).map(|(x, y)| (0..3).map(|c| (x[c] - y[c]).abs()).fold(0f32, f32::max)).fold(0f32, f32::max);
            let cached = rawdev::merged::read_preview(&key);
            let shown = self.a.shown_merged().map(|(_, d)| (d.merged.as_ref().map(|m| m.source.key.clone()), maxd(&d.preview.px, &want.px), d.preview.px.len()));
            eprintln!("      oracle: key {key} cache-vs-fresh {:?} shown-vs-fresh {:?} fresh len {} sources {:?}", cached.map(|c| maxd(&c.px, &want.px)), shown, want.px.len(), recipe.sources.iter().map(|s| s.file).collect::<Vec<_>>());
        }
        let stale_or_gone = self.a.merge_info().is_none_or(|i| i.stale || i.pending) || self.interrupted;
        if self.shown_hdr() && !stale_or_gone {
            let (_, doc) = self.a.shown_merged().unwrap();
            if doc.merged.as_ref().is_some_and(|m| m.source.key == key) && differs(&doc.preview.px, &want.px) {
                let maxd = doc.preview.px.iter().zip(&want.px).map(|(x, y)| (0..3).map(|c| (x[c] - y[c]).abs()).fold(0f32, f32::max)).fold(0f32, f32::max);
                let cache = rawdev::merged::read_preview(&key).map(|c| c.px.iter().zip(&want.px).map(|(x, y)| (0..3).map(|c| (x[c] - y[c]).abs()).fold(0f32, f32::max)).fold(0f32, f32::max));
                let over: Vec<(f32, f32)> = doc.preview.px.iter().zip(&want.px).flat_map(|(x, y)| (0..3).map(move |c| (x[c], y[c]))).filter(|(x, y)| (x - y).abs() > 2e-3 * (1.0 + x.abs())).collect();
                eprintln!("      pixels over the tolerance: {} values, e.g. {:?}", over.len(), over.iter().take(6).collect::<Vec<_>>());
                return Err(format!("the photo shown has other pixels than a fresh merge of its recipe (largest difference {maxd}, cache against fresh {cache:?}, {} px, sources {:?}, {})", want.px.len(), recipe.sources.iter().map(|s| s.file).collect::<Vec<_>>(), recipe.settings));
            }
        }
        if let Some(have) = rawdev::merged::read_preview(&key) {
            if differs(&have.px, &want.px) {
                return Err(format!("the cached preview {key} is not the merge of the recipe ({})", recipe.settings));
            }
        }
        if let Some(have) = rawdev::merged::read_full(&key) {
            if differs(&have.px, &want_full.px) {
                return Err(format!("the cached result {key} is not the merge of the recipe ({})", recipe.settings));
            }
        }
        // the photo is left (it was worked on in the tool): only the stored state stays
        self.click("nothing", None);
        self.settle();
        if self.verbose {
            eprintln!("      before leaving: tool {:?} right {} grid {} live bytes {} shown {} keys {:?}", self.a.tool_rail_active(), self.a.show_right, self.a.in_grid(), rawdev::merged::live_bytes(), self.shown_hdr(), cache_keys(file));
        }
        self.a.to_grid();
        self.step(905, Act::Wait(2))?;
        self.step(906, Act::Wait(7))?;
        // (the cache is tidied beside the app: a moment for it on a busy computer)
        let t = std::time::Instant::now();
        while t.elapsed() < std::time::Duration::from_secs(5) {
            let (results, bases) = cache_keys(file);
            if !(results.iter().any(|k| *k != key) || results.len() > 1 || bases.len() > 1) {
                break;
            }
            self.frame();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (results, bases) = cache_keys(file);
        if results.iter().any(|k| *k != key) || results.len() > 1 || bases.len() > 1 {
            return Err(format!("after leaving the photo the cache holds results {results:?} and bases {bases:?}; the recipe is {key}"));
        }
        Ok(())
    }

    /// Let everything run out and close (also after a failure).
    fn close(mut self) {
        crate::hdr_ui::gate::set(false);
        self.act(Act::SourceBack);
        let t = std::time::Instant::now();
        while (!crate::hdr_ui::gate::jobs().is_empty() || crate::hdr_ui::gate::busy() > 0) && t.elapsed() < std::time::Duration::from_secs(20) {
            self.frame();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        rawdev::merged::set_live(None);
        library::store::flush(std::time::Duration::from_secs(10));
        // the bracket leaves the library again (the next sequence starts with an empty one)
        let dir = self.dir.clone();
        drop(self.a);
        let db = Db::open(&library::db_path()).unwrap();
        for f in db.conn.prepare("SELECT file FROM merge").unwrap().query_map([], |r| r.get::<_, i64>(0)).unwrap().flatten() {
            rawdev::merged::forget_file(f);
        }
        if let Ok(root) = db.conn.query_row("SELECT id FROM root_folder WHERE path = ?1", [format!("{}/", dir.display())], |r| r.get::<_, i64>(0)) {
            let _ = library::manage::remove_folder(&db, root, "");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Do two results of the same recipe differ? Values agree within 0.2 % (of 1 + value; results are stored as half
/// floats). (For a while one value of a preview was allowed to be off by 0.004: a result made from the base in memory
/// – unrounded – and one made from the base read back from the cache – half floats – differed where a decision tipped.
/// The base is rounded as it is stored now, before anything is made from it.)
fn pixels_differ(a: &[[f32; 3]], b: &[[f32; 3]]) -> bool {
    a.iter().zip(b).any(|(x, y)| (0..3).any(|c| (x[c] - y[c]).abs() > 2e-3 * (1.0 + x[c].abs())))
}

/// Keys of the cache files of the merged file: (results, bases).
fn cache_keys(file: i64) -> (Vec<String>, Vec<String>) {
    let prefix = library::merge::key_prefix(file);
    let (mut results, mut bases) = (Vec::new(), Vec::new());
    for e in std::fs::read_dir(rawdev::merged::cache_dir()).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) || name.len() < prefix.len() + 16 || name.ends_with(".tmp") {
            continue;
        }
        let key = name[..prefix.len() + 16].to_string();
        let list = if name[prefix.len() + 16..].starts_with("-base") { &mut bases } else { &mut results };
        if !list.contains(&key) {
            list.push(key);
        }
    }
    results.sort();
    bases.sort();
    (results, bases)
}

/// The statics of a session as after a start of the app.
fn new_session() {
    crate::hdr_ui::test_new_session();
    crate::hdr_live::test_new_session();
    crate::hdr_auto::test_new_session();
    crate::hdr_paint::test_new_session();
    crate::hdr_batch::test_new_session();
    super::test_new_session();
    rawdev::merged::set_live(None);
    while rawdev::merged::cache_busy() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Run one sequence; `Err((step, why, trace))` when a check fails or something panics.
fn run(acts: &[Act], verbose: bool) -> Result<(), (usize, String, Vec<String>)> {
    run_then(acts, verbose, None)
}

/// [`run`]; with `end` the sequence ends with that check instead of the usual end of a sequence.
fn run_then(acts: &[Act], verbose: bool, end: Option<&dyn Fn(&mut Lab) -> Check>) -> Result<(), (usize, String, Vec<String>)> {
    let mut lab = Lab::new(verbose);
    let mut failed = None;
    for (n, act) in acts.iter().enumerate() {
        if lab.over {
            break;
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lab.step(n, *act)));
        match r {
            Ok(Ok(())) => {}
            Ok(Err(why)) => {
                failed = Some((n, why));
                break;
            }
            Err(p) => {
                let why = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                failed = Some((n, format!("panic: {why}")));
                break;
            }
        }
    }
    if failed.is_none() && !lab.over {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match end {
            Some(f) => f(&mut lab),
            None => lab.finish(),
        })) {
            Ok(Ok(())) => {}
            Ok(Err(why)) => failed = Some((acts.len(), why)),
            Err(p) => failed = Some((acts.len(), format!("panic: {}", p.downcast_ref::<String>().cloned().unwrap_or_default()))),
        }
    }
    let trace = std::mem::take(&mut lab.trace);
    PANICS.store(0, Ordering::SeqCst);
    lab.close();
    match failed {
        None => Ok(()),
        Some((n, why)) => Err((n, why, trace)),
    }
}

/// A shorter sequence that fails the same way (steps removed one at a time, a bounded number of tries).
fn shorten(acts: &[Act], why: &str) -> Vec<Act> {
    let class = |w: &str| w.chars().filter(|c| !c.is_ascii_digit()).take(40).collect::<String>();
    let mut best = acts.to_vec();
    let mut tries = 0;
    let mut i = best.len();
    while i > 1 && tries < 25 {
        i -= 1;
        let mut shorter = best.clone();
        shorter.remove(i);
        tries += 1;
        if let Err((n, w, _)) = run(&shorter, false) {
            if class(&w) == class(why) {
                shorter.truncate((n + 1).min(shorter.len()));
                best = shorter;
                i = i.min(best.len());
            }
        }
    }
    best
}

fn with_lab_lock<T>(f: impl FnOnce() -> T) -> T {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    crate::hdr_ui::clock::set_virtual(true);
    let r = f();
    crate::hdr_ui::clock::set_virtual(false);
    crate::hdr_ui::gate::set(false);
    r
}

/// Random sequences (see the module docs for the switches).
#[test]
fn random_sequences_keep_the_states_together() {
    let spec = std::env::var("GRAINEO_HDR_STRESS").unwrap_or_else(|_| "25x30".into());
    let (count, rest) = spec.split_once('x').unwrap_or(("25", "30"));
    let (steps, first) = rest.split_once('@').unwrap_or((rest, "1"));
    let (count, steps, first): (u64, usize, u64) = (count.parse().unwrap(), steps.parse().unwrap(), first.parse().unwrap());
    let one: Option<u64> = std::env::var("GRAINEO_HDR_STRESS_SEED").ok().and_then(|s| s.parse().ok());
    with_lab_lock(|| {
        let seeds: Vec<u64> = one.map_or_else(|| (first..first + count).collect(), |s| vec![s]);
        let t = std::time::Instant::now();
        let mut failures = Vec::new();
        for seed in &seeds {
            let acts = sequence(*seed, steps);
            let t1 = std::time::Instant::now();
            let r = run(&acts, one.is_some());
            if std::env::var_os("GRAINEO_HDR_STRESS_TIMES").is_some() {
                eprintln!("seed {seed}: {:.2} s", t1.elapsed().as_secs_f32());
            }
            if let Err((n, why, trace)) = r {
                eprintln!("\nHDR STRESS seed {seed} ({steps} steps) failed at step {n}: {why}");
                for l in &trace {
                    eprintln!("  {l}");
                }
                let short = if std::env::var_os("GRAINEO_HDR_STRESS_NO_SHORTEN").is_some() { acts[..(n + 1).min(acts.len())].to_vec() } else { shorten(&acts[..(n + 1).min(acts.len())], &why) };
                eprintln!("  shortest failing sequence found ({} steps): {short:?}", short.len());
                failures.push((*seed, why));
                if failures.len() >= 5 {
                    break;
                }
            }
        }
        eprintln!("hdr stress: {} sequences × {steps} steps in {:.1} s, {} failed", seeds.len(), t.elapsed().as_secs_f32(), failures.len());
        assert!(failures.is_empty(), "failing seeds: {:?}", failures);
    });
}

/// A fixed sequence has to pass every check (regression tests of what the random sequences found).
fn passes(acts: &[Act]) {
    with_lab_lock(|| {
        if let Err((n, why, trace)) = run(acts, false) {
            panic!("step {n}: {why}\n{}", trace.join("\n"));
        }
    });
}

/// Seed 15 (H1 of the second audit): a merged photo made after another one was removed got its file id and with it
/// the tool's recipe of the removed one – the first change stored a reference outside its two frames.
#[test]
fn a_new_merge_inherits_nothing_from_a_removed_one() {
    use Act::*;
    passes(&[Create(0b0001110), JobsFinish, Reference(2), JobsFinish, Paint(3), JobsFinish, Wait(70), Remove(true), Create(0b0010010), JobsFinish, Paint(32), JobsFinish, Wait(70), Strength(40), Wait(70)]);
}

/// Seed 24: a develop slider moved, then "HDR entfernen" before the change was written – ⌘Z brought the photo back
/// without the development (the write arrived after the photo had left and was dropped).
#[test]
fn a_development_on_its_way_survives_remove_and_undo() {
    use Act::*;
    passes(&[Create(0b0011100), JobsFinish, Develop(16), Remove(true), Wait(7), Undo, JobsFinish, Photo(7), Wait(70)]);
}

/// A fixed sequence passes every check of the steps and then `end`.
fn passes_then(acts: &[Act], end: &dyn Fn(&mut Lab) -> Check) {
    with_lab_lock(|| {
        if let Err((n, why, trace)) = run_then(acts, false, Some(end)) {
            panic!("step {n}: {why}\n{}", trace.join("\n"));
        }
    });
}

impl Lab {
    /// The status line of the tool for the merged photo shown.
    fn status(&self) -> String {
        self.a.merge_info().map(|i| crate::hdr_paint::test_status(&i).0).unwrap_or_default()
    }
}

/// M1 of the second audit: a merged photo opened without its cached result whose merge fails every time (a source is
/// damaged) was computed again and again, as long as it was open. Now: tried once, then "Nicht aktuell" with the
/// reason and "Neu berechnen" – which tries once more.
#[test]
fn a_merge_that_keeps_failing_is_not_tried_for_ever() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70), Photo(0), Wait(7), SourceBroken(4), ClearCache, Restart, JobsFinish, Wait(70), JobsFinish, Wait(70)], &|lab| {
        let before = crate::hdr_ui::gate::started();
        for _ in 0..40 {
            lab.frame();
        }
        lab.finish_jobs();
        let started = crate::hdr_ui::gate::started() - before;
        if started > 0 {
            return Err(format!("{started} jobs started by themselves for a result that failed"));
        }
        let line = lab.status();
        if !line.starts_with("Nicht aktuell") {
            return Err(format!("status \"{line}\""));
        }
        // "Neu berechnen" tries again (once), and with the source repaired the photo gets its picture
        lab.step(100, Recompute)?;
        lab.step(101, JobsFinish)?;
        // (once; a second job may follow when the automatic tone stores its values with the same click – never a series)
        if !(1..=2).contains(&(crate::hdr_ui::gate::started() - before)) {
            return Err(format!("\"Neu berechnen\" started {} jobs", crate::hdr_ui::gate::started() - before));
        }
        lab.act(SourceBack);
        let before = crate::hdr_ui::gate::started();
        lab.step(102, Recompute)?;
        lab.step(103, JobsFinish)?;
        lab.step(104, Wait(70))?;
        for _ in 0..40 {
            lab.frame();
        }
        if crate::hdr_ui::gate::started() - before > 3 {
            return Err(format!("{} jobs after the repair", crate::hdr_ui::gate::started() - before));
        }
        let line = lab.status();
        if !line.starts_with("Aktuell") {
            return Err(format!("after the repair the status is \"{line}\""));
        }
        Ok(())
    });
}

/// M8: without its cached result and with a source out of reach the photo said "Wartet auf die Berechnung …" for
/// ever. Now it names the source; and "Leeren" of the cache spares a photo whose sources are out of reach.
#[test]
fn a_missing_source_is_named_and_its_result_is_spared() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70), Photo(0), Wait(7), SourceGone(4), ClearCache], &|lab| {
        let file = lab.hdr.ok_or("no merged photo")?;
        if cache_keys(file).0.is_empty() {
            return Err("\"Leeren\" removed the result of a photo that cannot be merged again now".into());
        }
        // the cache lost by other means: the photo opens with its reference and says why it stays that way
        rawdev::merged::forget_file(file);
        lab.step(100, Restart)?;
        lab.step(101, JobsFinish)?;
        lab.step(102, Wait(70))?;
        let line = lab.status();
        if !line.starts_with("Quelle fehlt: S0005.dng") {
            return Err(format!("status \"{line}\""));
        }
        Ok(())
    });
}

/// QA 11, M-3: a source renamed and renamed back while the app was closed: the photo opens while the library still
/// has the other name (the source "not reachable"), then the library follows the file. The tool took the paths of the
/// document opened before and stayed locked until the photo was opened again; now it reads them from the library.
#[test]
fn a_source_renamed_back_unlocks_the_tool_without_reopening() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70), Photo(0), Wait(7)], &|lab| {
        let (file, _, path) = lab.frames[3].clone();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let set_name = |lab: &Lab, n: &str| lab.db().conn.execute(&format!("UPDATE file SET name = '{}' WHERE id = {file}", n.replace('\'', "''")), []).map_err(|e| e.to_string());
        // the library still names the source as it was renamed: the photo opens with that path
        set_name(lab, "S0004x.dng")?;
        lab.step(100, Restart)?;
        set_name(lab, "S0004x.dng")?;
        lab.a.to_single_image();
        if let Some(i) = lab.hdr_entry() {
            lab.a.select(i, false);
        }
        lab.step(101, Wait(70))?;
        let info = lab.a.merge_info().ok_or("not shown")?;
        if info.missing.is_empty() {
            return Err("the source under its old name counts as reachable".into());
        }
        // the library follows the file
        set_name(lab, &name)?;
        crate::hdr_paint::recipe_changed();
        lab.step(102, Wait(70))?;
        let info = lab.a.merge_info().ok_or("not shown")?;
        if !info.missing.is_empty() || !info.in_step {
            return Err(format!("after the library followed the file: missing {:?}, in step {}", info.missing, info.in_step));
        }
        Ok(())
    });
}

/// M7: a change of the frames that was cancelled left the list locked ("… ändern geht gleich wieder") although nothing
/// computed. Now the tool says what is the case, and "Auswahl zurücknehmen" unlocks the list.
#[test]
fn a_cancelled_change_of_the_frames_has_a_way_out() {
    use Act::*;
    passes_then(&[Create(0b0111110), JobsFinish, Wait(70), Tick(0), Cancel, JobsFinish, Wait(70)], &|lab| {
        let info = lab.a.merge_info().ok_or("not shown")?;
        if info.in_step || info.busy || !info.stale {
            return Err(format!("expected a locked list that is out of date: in step {}, busy {}, out of date {}", info.in_step, info.busy, info.stale));
        }
        lab.step(100, Restore)?;
        lab.step(101, JobsFinish)?;
        lab.step(102, Wait(70))?;
        let info = lab.a.merge_info().ok_or("not shown")?;
        let line = lab.status();
        if !info.in_step || !line.starts_with("Aktuell") || info.sources.len() != 5 {
            return Err(format!("after \"Auswahl zurücknehmen\": in step {}, status \"{line}\", {} frames", info.in_step, info.sources.len()));
        }
        // and the tick works again
        lab.step(103, Tick(0))?;
        lab.step(104, JobsFinish)?;
        lab.step(105, Wait(70))?;
        if lab.recipe().map(|r| r.0.sources.len()) != Some(4) {
            return Err("the tick did not change the recipe".into());
        }
        Ok(())
    });
}

/// M5: ⌘Z while the job that creates the photo is past its last check (the result is being stored) used up the step
/// and left the photo. Now the step is carried out when the job has ended.
#[test]
fn undo_at_the_very_end_of_the_first_computation_still_removes() {
    use Act::*;
    passes_then(&[Create(0b0011100)], &|lab| {
        let file = lab.hdr.ok_or("no merged photo")?;
        // the job to its end point: computed, not yet said so
        for _ in 0..3 {
            let j = crate::hdr_ui::gate::jobs().first().cloned().ok_or("no job")?;
            crate::hdr_ui::gate::step(j.id);
            lab.settle();
        }
        if !crate::hdr_ui::creating(file) {
            return Err("the job is not at its end point".into());
        }
        // ⌘Z of the library (the step "HDR zusammenfügen")
        lab.may_vanish = true;
        let a = lab.a.lib.undo_external();
        lab.a.library_actions(a);
        lab.settle();
        if lab.merged_files().is_empty() {
            return Err("the photo left before its job ended".into());
        }
        lab.finish_jobs();
        lab.settle();
        if !lab.merged_files().is_empty() {
            return Err("⌘Z was used up, the photo is still there".into());
        }
        lab.hdr = None;
        Ok(())
    });
}

/// Seeds 278/316: a result handed over from the cache delivers its preview and its full resolution in one go. For a
/// photo still standing in with its reference frame, the document built for the full resolution kept the preview of
/// that reference and counted as the merge ("Aktuell" over the pixels of a single exposure). Run a few times: it
/// depended on which of the two documents was ready first.
#[test]
fn a_cached_result_never_leaves_the_reference_as_the_preview() {
    use Act::*;
    for _ in 0..4 {
        // computed while the photo is not shown, then shown again from the app's kept (waiting) document
        passes(&[Create(0b0111000), Photo(0), JobsFinish, Photo(7), JobsFinish, Wait(70), Ghost(0), Photo(0), JobsFinish, Photo(7), JobsFinish, Wait(70)]);
    }
}

// ─────────────────────────────────────────────── the batch "Alle Reihen zusammenfügen"

/// One action of a batch sequence (numbers modulo what is there, impossible actions do nothing).
#[derive(Clone, Copy, Debug, PartialEq)]
enum BAct {
    /// "Alle Reihen zusammenfügen" for these brackets (bit mask of the three); while a batch runs: a second start
    Start(usize),
    /// a second start while a batch runs (the dialog and the library both have to refuse)
    StartAgain,
    JobStep(usize),
    JobsFinish,
    /// open the merged photo of this bracket (waiting, running or done)
    Open(usize),
    /// show a source frame instead
    Leave,
    Paint(usize),
    Ghost(usize),
    Develop(usize),
    /// "Abbrechen" in the title bar
    CancelAll,
    /// ⌘Z; the question about own work is answered with yes (`true`) or cancelled
    Undo(bool),
    Redo,
    /// the app is quit while the batch runs, and started again
    Kill,
    /// the app is quit at rest and started again
    Restart,
    /// export the merged photo of this bracket (beside whatever runs)
    Export(usize),
    NewPhoto,
    Wait(usize),
}

fn random_bact(r: &mut Rng) -> BAct {
    match r.below(100) {
        0..=7 => BAct::Start(1 + r.below(7)),
        8..=11 => BAct::StartAgain,
        12..=31 => BAct::JobStep(r.below(3)),
        32..=37 => BAct::JobsFinish,
        38..=49 => BAct::Open(r.below(3)),
        50..=52 => BAct::Leave,
        53..=60 => BAct::Paint(r.below(35)),
        61..=64 => BAct::Ghost(r.below(3)),
        65..=68 => BAct::Develop(1 + r.below(40)),
        69..=75 => BAct::CancelAll,
        76..=83 => BAct::Undo(r.below(4) != 0),
        84..=85 => BAct::Redo,
        86..=88 => BAct::Kill,
        89 => BAct::Restart,
        90..=93 => BAct::Export(r.below(3)),
        94..=95 => BAct::NewPhoto,
        _ => BAct::Wait([2, 7, 35, 70][r.below(4)]),
    }
}

fn bsequence(seed: u64, steps: usize) -> Vec<BAct> {
    let mut r = Rng::new(seed ^ 0xBA7C_0000);
    // every sequence starts with a batch of at least two brackets
    let mut v = vec![BAct::Start([3, 5, 6, 7][r.below(4)])];
    v.extend((1..steps).map(|_| random_bact(&mut r)));
    v
}

/// The model of a batch sequence around a [`Lab`] with three brackets.
struct Batch {
    lab: Lab,
    /// merged photos of the run started last, in its order
    run: Vec<i64>,
    /// merged photos the library holds (as of the last step), with what was seen of them then: (file, a result that
    /// can be shown, work of the user)
    present: Vec<(i64, bool, bool)>,
    /// a stop was asked for (title bar, ⌘Z while running, quit) and the batch has not come to rest since
    stop_asked: bool,
    /// ⌘Z of the batch was accepted (no question, or answered with yes) and is not carried out to its end yet
    undo_asked: bool,
    /// the ⌘Z step of the run exists in this session
    step_alive: bool,
    /// stacks of the user over the frames before the run started
    user_stacks: i64,
    /// position of the batch seen last
    last_pos: Option<usize>,
    /// exports under way: (file, its document when done)
    exports: Vec<(i64, Arc<Mutex<Option<Result<(String, bool), String>>>>)>,
    killed: bool,
}

impl Batch {
    fn new(verbose: bool) -> Batch {
        let lab = Lab::open(verbose, batch_templates(), &batch_names());
        Batch { lab, run: Vec::new(), present: Vec::new(), stop_asked: false, undo_asked: false, step_alive: false, user_stacks: 0, last_pos: None, exports: Vec::new(), killed: false }
    }

    /// Files of the frames of bracket `g`.
    fn group(&self, g: usize) -> Vec<i64> {
        self.lab.frames[3 * (g % 3)..3 * (g % 3) + 3].iter().map(|f| f.0).collect()
    }

    fn requests(&self, mask: usize) -> Vec<crate::hdr_ui::Request> {
        (0..3)
            .filter(|g| mask >> g & 1 == 1)
            .filter_map(|g| crate::hdr_ui::request(self.lab.a.lib.hdr_rows_of_files(&self.group(g))).ok())
            .map(|req| crate::hdr_ui::Request { stack: true, keyword: "HDR".into(), ..req })
            .collect()
    }

    /// The merged photo of the run made of bracket `g`, if the library has it.
    fn item(&self, g: usize) -> Option<i64> {
        let db = self.lab.db();
        let frames = self.group(g);
        self.present.iter().map(|p| p.0).find(|f| library::merge::recipe(&db.conn, *f).is_some_and(|r| r.sources.iter().all(|s| frames.contains(&s.file))))
    }

    fn user_stacks(&self) -> i64 {
        let db = self.lab.db();
        let images: Vec<String> = self.lab.frames.iter().map(|f| f.1.to_string()).collect();
        db.conn.query_row(&format!("SELECT count(DISTINCT s.id) FROM stack s JOIN stack_image si ON si.stack = s.id WHERE s.origin = 'user' AND si.image IN ({})", images.join(",")), [], |r| r.get(0)).unwrap_or(-1)
    }

    fn batch_steps(&self) -> usize {
        self.lab.a.lib.manage.undo.iter().filter(|u| matches!(u, library_manage::Undo::MergeBatch { .. })).count()
    }

    fn rest(&self) -> bool {
        use crate::hdr_ui::gate;
        gate::jobs().is_empty() && rawdev::merged::jobs().is_empty() && !self.lab.busy() && crate::hdr_ui::turn::queue().is_empty()
    }

    fn open_photo(&mut self, file: Option<i64>) {
        self.lab.a.to_single_image();
        match file.and_then(|f| self.lab.a.entries.iter().position(|e| e.file == f)) {
            Some(i) => {
                self.lab.a.lib.set_active(self.lab.a.entries[i].image);
                self.lab.a.select(i, false);
            }
            None => self.lab.a.to_grid(),
        }
        self.lab.hdr = file.filter(|f| self.present.iter().any(|p| p.0 == *f));
        self.lab.hdr_path = self.lab.hdr.and_then(|f| library::merge::file_path(&self.lab.db().conn, f));
    }

    fn act(&mut self, act: BAct) {
        use crate::hdr_ui::gate;
        match act {
            BAct::Start(mask) => {
                if crate::hdr_batch::active() {
                    return self.act(BAct::StartAgain);
                }
                if !self.present.is_empty() || !self.rest() {
                    return;
                }
                let reqs = self.requests(mask);
                if reqs.is_empty() {
                    return;
                }
                self.user_stacks = self.user_stacks();
                self.lab.a.to_grid();
                self.lab.a.lib.start_merge_batch(reqs);
            }
            BAct::StartAgain => {
                if !crate::hdr_batch::active() {
                    return;
                }
                // the dialog does not open, and a request that got past it is refused by the library
                crate::hdr_batch::open(Vec::new());
                if crate::hdr_batch::is_open() {
                    self.lab.note("the dialog of a second batch opened".into());
                    self.lab.over = true;
                }
                let reqs = self.requests(1);
                self.lab.a.lib.start_merge_batch(reqs);
            }
            BAct::JobStep(k) => {
                let waiting: Vec<gate::Job> = gate::jobs().into_iter().filter(|j| j.at.is_some()).collect();
                if let Some(j) = waiting.get(k % waiting.len().max(1)) {
                    self.lab.note(format!("job {} of {} ({}) passes {:?}", j.id, j.file, if j.update { "change" } else { "create" }, j.at));
                    gate::step(j.id);
                }
            }
            BAct::JobsFinish => self.lab.finish_jobs(),
            BAct::Open(g) => {
                let f = self.item(g);
                if f.is_some() {
                    self.open_photo(f);
                }
            }
            BAct::Leave => {
                self.lab.a.to_single_image();
                if let Some(i) = self.lab.a.entries.iter().position(|e| e.file == self.lab.frames[0].0) {
                    self.lab.a.lib.set_active(self.lab.a.entries[i].image);
                    self.lab.a.select(i, false);
                } else {
                    self.lab.a.to_grid();
                }
                self.lab.hdr = None;
            }
            BAct::Paint(seed) => self.lab.click("paint", Some(seed)),
            BAct::Ghost(k) => self.lab.click("ghost", Some(k)),
            BAct::Develop(v) => {
                if self.lab.shown_hdr() {
                    if let Some(i) = self.lab.hdr_entry() {
                        self.lab.a.entries[i].settings.contrast = v as f64 / 100.0;
                        self.lab.a.save_current();
                    }
                }
            }
            BAct::CancelAll => {
                self.stop_asked = true;
                crate::hdr_ui::cancel_all();
            }
            BAct::Undo(yes) => {
                let before = self.batch_steps();
                let running = crate::hdr_batch::active();
                let removals = |b: &Batch| b.lab.a.lib.manage.undo.iter().filter(|u| matches!(u, library_manage::Undo::MergeRemoved { .. })).count();
                let removed_before = removals(self);
                self.lab.a.undo_latest();
                self.lab.frame();
                self.lab.settle();
                // this ⌘Z took back a removal (the step "HDR entfernen" a ⌘Z of the batch left for photos with work):
                // the photos are back on purpose (seed 2700128: the ⌘Z that waited for the batch was carried out when
                // the batch ended, the next ⌘Z brought the worked-on photo back)
                if removals(self) < removed_before {
                    self.undo_asked = false;
                }
                let asked = matches!(self.lab.a.lib.manage.ask, Some(library_manage::Ask::RemoveMerge { undo: true, .. }));
                let hit = asked || self.lab.a.lib.manage.batch_undo_waiting || self.batch_steps() < before;
                if asked {
                    self.lab.answer_ask(yes);
                }
                if hit && (!asked || yes) {
                    self.undo_asked = true;
                    self.stop_asked |= running;
                }
            }
            BAct::Redo => self.lab.a.redo_latest(),
            BAct::Kill => {
                // (what the user did to the open photo is saved on quitting: seen before the model decides what may go)
                self.lab.a.save_current();
                library::store::flush(std::time::Duration::from_secs(10));
                for p in &mut self.present {
                    p.2 = super::has_own_work(&self.lab.db(), p.0);
                }
                self.stop_asked = true;
                self.killed = true;
                self.step_alive = false;
                self.undo_asked = false;
                self.last_pos = None;
                self.lab.kill();
                self.lab.hdr = None;
            }
            BAct::Restart => {
                if !self.rest() || crate::hdr_batch::active() {
                    return;
                }
                self.step_alive = false;
                self.undo_asked = false;
                self.lab.act(Act::Restart);
                self.lab.hdr = None;
            }
            BAct::Export(g) => {
                let Some(file) = self.item(g) else { return };
                let Some(path) = library::merge::file_path(&self.lab.db().conn, file) else { return };
                let done = Arc::new(Mutex::new(None));
                let out = done.clone();
                crate::hdr_ui::spawn(move || {
                    let r = rawdev::RawDoc::open_for_output(&path, &crate::Settings::default(), &|| true).and_then(|d| d.ok_or_else(|| "abgebrochen".to_string())).and_then(|d| {
                        let m = d.merged.clone().ok_or_else(|| "not a merged photo".to_string())?;
                        Ok((m.source.key.clone(), m.pending))
                    });
                    *out.lock().unwrap() = Some(r);
                });
                self.exports.push((file, done));
            }
            BAct::NewPhoto => self.lab.new_photo(),
            BAct::Wait(tenths) => crate::hdr_ui::clock::advance(std::time::Duration::from_millis(100 * tenths as u64)),
        }
    }

    fn step(&mut self, n: usize, act: BAct) -> Check {
        if self.lab.verbose {
            eprintln!("{n:3} {act:?}");
        }
        self.lab.trace.push(format!("{n:3} {act:?}"));
        // jobs told to stop do not wait for ever: with more than three waiting the oldest runs on
        let waiting = crate::hdr_ui::gate::jobs();
        if waiting.len() > 3 {
            for j in &waiting[..waiting.len() - 3] {
                self.lab.note(format!("job {} runs to its end", j.id));
                crate::hdr_ui::gate::finish(j.id);
            }
            self.lab.settle();
        }
        self.act(act);
        self.lab.settle();
        if self.lab.verbose {
            let m = &self.lab.a.lib.manage;
            let steps: Vec<String> = m
                .undo
                .iter()
                .map(|u| match u {
                    library_manage::Undo::Merge { file, .. } => format!("Merge({file})"),
                    library_manage::Undo::MergeRemoved { files, .. } => format!("MergeRemoved({files:?})"),
                    library_manage::Undo::MergeBatch { items, .. } => format!("MergeBatch({:?})", items.iter().map(|i| i.0).collect::<Vec<_>>()),
                    _ => "other".into(),
                })
                .collect();
            eprintln!("      ⌘Z steps {steps:?}, batch ⌘Z waits {}, confirmed {:?}, question {}, batch active {}, in the library {:?}, jobs {:?}", m.batch_undo_waiting, m.merge_undo_confirmed, m.ask.is_some(), crate::hdr_batch::active(), self.lab.merged_files(), crate::hdr_ui::gate::jobs().iter().map(|j| (j.id, j.file, j.at)).collect::<Vec<_>>());
        }
        // a question nobody asked for with this step (a ⌘Z that waited for the batch meets work made meanwhile): the
        // user closes it, the step stays
        if !matches!(act, BAct::Undo(_)) && matches!(self.lab.a.lib.manage.ask, Some(library_manage::Ask::RemoveMerge { undo: true, .. })) {
            self.lab.answer_ask(false);
            self.undo_asked = false;
        }
        if self.lab.over {
            return Err("a second batch was not refused".into());
        }
        self.follow(act)?;
        self.check(act)
    }

    /// The library against the model: merged photos appear and leave only for a reason.
    fn follow(&mut self, act: BAct) -> Check {
        // (what the app holds of the open photo goes to the library first: the work seen is the work there is)
        self.lab.a.save_current();
        library::store::flush(std::time::Duration::from_secs(10));
        let now = self.lab.merged_files();
        let db = self.lab.db();
        let new: Vec<i64> = now.iter().copied().filter(|f| !self.present.iter().any(|p| p.0 == *f)).collect();
        let created: Vec<i64> = new.iter().copied().filter(|f| !self.lab.ids.contains(f)).collect();
        if !created.is_empty() {
            if !matches!(act, BAct::Start(_)) {
                return Err(format!("merged photos {created:?} appeared after {act:?}"));
            }
            if crate::hdr_batch::test_state().0 != created && crate::hdr_batch::active() {
                return Err(format!("a batch created {created:?}, the batch running works on {:?}", crate::hdr_batch::test_state().0));
            }
            if let Some(f) = created.iter().find(|f| self.lab.ids.iter().any(|old| *f <= old)) {
                return Err(format!("the new merged photo got the file id {f}; earlier ones had {:?}", self.lab.ids));
            }
            for f in &created {
                self.lab.ids.push(*f);
                if let Ok(image) = db.conn.query_row("SELECT id FROM image WHERE file = ?1 AND copy IS NULL", [f], |r| r.get::<_, i64>(0)) {
                    self.lab.image_ids.push(image);
                }
            }
            self.run = created.clone();
            self.step_alive = true;
            self.killed = false;
            self.last_pos = None;
        }
        for f in &new {
            if !created.contains(f) {
                if !matches!(act, BAct::Undo(_)) {
                    return Err(format!("the merged photo {f} came back after {act:?}"));
                }
                self.lab.note(format!("⌘Z brought the merged photo {f} back"));
                // (the step of the batch was carried out before: nothing of it is owed any more)
                self.undo_asked = false;
                self.step_alive = false;
                self.run.clear();
            }
            self.present.push((*f, false, false));
        }
        for (f, result, work) in std::mem::take(&mut self.present) {
            if now.contains(&f) {
                self.present.push((f, result, work));
                continue;
            }
            // gone: taken back by ⌘Z, or – after a stop – one that had neither a result nor work
            let abandoned = !result && !work && self.stop_asked;
            if !(self.undo_asked || abandoned || matches!(act, BAct::Redo)) {
                return Err(format!("the merged photo {f} left the library after {act:?} (result {result}, work {work}, stop asked {}, ⌘Z accepted {})", self.stop_asked, self.undo_asked));
            }
            self.lab.note(format!("merged photo {f} left the library"));
            if self.lab.hdr == Some(f) {
                self.lab.hdr = None;
            }
        }
        drop(db);
        // what is seen of the photos now (for the next step)
        for p in &mut self.present {
            p.1 = self.lab.has_result(p.0);
            p.2 = self.lab.has_work(p.0);
        }
        if matches!(act, BAct::Kill) {
            let db = self.lab.db();
            let left = library::merge::unfinished(&db.conn);
            if !left.is_empty() {
                return Err(format!("after the restart merged photos still count as unfinished: {:?}", left.iter().map(|u| u.file).collect::<Vec<_>>()));
            }
            for f in std::mem::take(&mut self.lab.killed_unfinished) {
                if library::merge::is_virtual(&db.conn, f) && !(rawdev::merged::has_cached(f) || library::preview::has_any_developed(f) || super::has_own_work(&db, f)) {
                    return Err(format!("the unfinished merged photo {f} was left in the library with nothing to show"));
                }
            }
        }
        // the sources are never touched
        let frames: i64 = self.lab.db().conn.query_row(&format!("SELECT count(*) FROM file WHERE status = 0 AND id IN ({})", self.lab.frames.iter().map(|f| f.0.to_string()).collect::<Vec<_>>().join(",")), [], |r| r.get(0)).unwrap_or(-1);
        if frames != self.lab.frames.len() as i64 {
            // what became of them (status per file, the folder on disk), so a failure names its cause
            let ids = self.lab.frames.iter().map(|f| f.0.to_string()).collect::<Vec<_>>().join(",");
            let db = self.lab.db();
            let states: Vec<String> = db.conn.prepare(&format!("SELECT id, status FROM file WHERE id IN ({ids})")).and_then(|mut st| st.query_map([], |r| Ok(format!("{}:{}", r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map(|r| r.flatten().collect())).unwrap_or_default();
            return Err(format!("{frames} of {} source frames are left in the library (file:status {states:?}, folder on disk {})", self.lab.frames.len(), self.lab.dir.is_dir()));
        }
        self.lab.check_new_photos()
    }

    fn check(&mut self, act: BAct) -> Check {
        use crate::hdr_ui::gate;
        if PANICS.load(Ordering::SeqCst) > 0 {
            return Err("a thread panicked".into());
        }
        // one merge at a time, whoever asks for it (the batch, a change, an export)
        let computing = rawdev::merged::jobs();
        if computing.len() > 1 {
            return Err(format!("{} merges are computed at once: {:?}", computing.len(), computing.iter().map(|j| j.file).collect::<Vec<_>>()));
        }
        if gate::jobs().len() > 8 {
            return Err(format!("{} jobs alive", gate::jobs().len()));
        }
        // the position shown: "Reihe k von n" of the run, for the photo being computed, never going back
        let (files, current) = crate::hdr_batch::test_state();
        if let Some(c) = current {
            if files != self.run {
                return Err(format!("the batch works on {files:?}, started was {:?}", self.run));
            }
            if self.last_pos.is_some_and(|l| c < l) {
                return Err(format!("the position of the batch went back from {:?} to {c}", self.last_pos));
            }
            self.last_pos = Some(c);
            for (k, f) in files.iter().enumerate() {
                let want = (k == c && files.len() > 1).then_some((c + 1, files.len()));
                if crate::hdr_batch::position(*f) != want {
                    return Err(format!("position of {f}: {:?}, expected {want:?}", crate::hdr_batch::position(*f)));
                }
            }
            if let Some(j) = computing.iter().find(|j| Some(&j.file) == files.get(c)).filter(|_| files.len() > 1) {
                let want = format!("{} von {}", c + 1, files.len());
                if !crate::hdr_ui::activities().iter().any(|a| a.contains(&want)) {
                    return Err(format!("the title bar does not say \"Reihe {want}\" for the merge of {}: {:?}", j.file, crate::hdr_ui::activities()));
                }
            }
        } else {
            self.last_pos = None;
            if let Some(f) = self.present.iter().map(|p| p.0).find(|f| crate::hdr_batch::position(*f).is_some() || crate::hdr_batch::pending(*f)) {
                return Err(format!("no batch runs, {f} still counts as part of one"));
            }
        }
        // exports that are done: never the reference frame standing in
        let mut i = 0;
        while i < self.exports.len() {
            let r = self.exports[i].1.lock().unwrap().take();
            match r {
                None => i += 1,
                Some(r) => {
                    let (file, _) = self.exports.remove(i);
                    match r {
                        Ok((key, pending)) => {
                            if pending {
                                return Err(format!("an export of {file} got the reference frame instead of the merge"));
                            }
                            if !key.starts_with(&library::merge::key_prefix(file)) {
                                return Err(format!("an export of {file} got the result {key}"));
                            }
                        }
                        Err(why) => self.lab.note(format!("export of {file}: {why}")),
                    }
                }
            }
        }
        if !self.rest() {
            return Ok(());
        }
        // (the library moved on since it was looked at: the next step follows it first)
        let now = self.lab.merged_files();
        if now.len() != self.present.len() || self.present.iter().any(|p| !now.contains(&p.0)) {
            return Ok(());
        }
        // at rest: nothing runs, nothing waits, nothing is locked
        if crate::hdr_batch::active() {
            return Err("no job runs, a batch still counts as running".into());
        }
        if self.lab.a.lib.manage.batch_undo_waiting || !self.lab.a.lib.manage.merge_undo_waiting.is_empty() {
            return Err("no job runs, a ⌘Z still waits for one".into());
        }
        for p in &self.present {
            if crate::hdr_ui::creating(p.0) || crate::hdr_ui::job_alive(p.0) {
                return Err(format!("no job runs, {} still counts as being computed", p.0));
            }
        }
        // ⌘Z of the batch, accepted and carried out: exactly the photos of the run are gone, the stacks are back
        if self.undo_asked && self.batch_steps() == 0 {
            if let Some(f) = self.run.iter().find(|f| self.present.iter().any(|p| p.0 == **f)) {
                return Err(format!("⌘Z of the batch left the merged photo {f} of the run {:?}", self.run));
            }
            if !self.killed && self.user_stacks() != self.user_stacks {
                return Err(format!("after ⌘Z of the batch {} stacks of the user hold the frames, before the batch {}", self.user_stacks(), self.user_stacks));
            }
            self.undo_asked = false;
            self.step_alive = false;
            self.run.clear();
        }
        self.stop_asked = false;
        // the photo shown: nothing to cancel, and after a while its result (or the honest reason why not)
        if self.lab.shown_hdr() {
            let Some(info) = self.lab.a.merge_info() else { return Err("the merged photo is shown, the tool does not know it".into()) };
            let (line, cancel) = crate::hdr_paint::test_status(&info);
            if cancel {
                return Err(format!("no job runs, the tool offers \"Abbrechen\" (\"{line}\")"));
            }
            if matches!(act, BAct::Wait(t) if t >= 70) && (info.pending || info.stale || info.failed.is_some()) && !(line.starts_with("Nicht aktuell") || line.starts_with("Quelle fehlt") || line.starts_with("Vorschau aktuell · volle Auflösung abgebrochen")) {
                return Err(format!("at rest the photo does not show its result (pending {}, out of date {}, failed {:?}), the status says \"{line}\"", info.pending, info.stale, info.failed));
            }
        }
        Ok(())
    }

    /// The end of a batch sequence: everything runs out; every merged photo left can be opened and shows its merge;
    /// the cached result of one of them is the merge of its recipe; ⌘Z of the run takes all of it back.
    fn finish(&mut self) -> Check {
        self.step(900, BAct::JobsFinish)?;
        self.step(901, BAct::Wait(70))?;
        self.step(902, BAct::JobsFinish)?;
        if !self.exports.is_empty() {
            return Err("an export never ended".into());
        }
        let mut compared = false;
        for g in 0..3 {
            let Some(file) = self.item(g) else { continue };
            self.step(910 + 10 * g, BAct::Open(g))?;
            self.step(911 + 10 * g, BAct::JobsFinish)?;
            self.step(912 + 10 * g, BAct::Wait(70))?;
            self.step(913 + 10 * g, BAct::JobsFinish)?;
            self.step(914 + 10 * g, BAct::Wait(70))?;
            if !self.lab.shown_hdr() {
                return Err(format!("the merged photo {file} cannot be shown"));
            }
            let line = self.lab.status();
            if !(line.starts_with("Aktuell") || line.starts_with("Vorschau aktuell") || line.starts_with("Nicht aktuell")) {
                return Err(format!("the merged photo {file} opened at the end says \"{line}\""));
            }
            if line.starts_with("Nicht aktuell") || compared {
                continue;
            }
            compared = true;
            let db = self.lab.db();
            let Some(recipe) = library::merge::recipe(&db.conn, file) else { continue };
            let key = recipe.cache_key(&db.conn);
            drop(db);
            let settings = rawdev::merged::parse_settings(&recipe.settings).map_err(|e| e.to_string())?;
            let sources: Vec<PathBuf> = recipe.sources.iter().map(|s| self.lab.frames.iter().find(|f| f.0 == s.file).unwrap().2.clone()).collect();
            let fresh = rawdev::merged::MergedSource::new(900_000_000 + file, sources, settings, format!("f{}-{:016x}", 900_000_000 + file, 1), format!("f{}-{:016x}", 900_000_000 + file, 2));
            let cancel = AtomicBool::new(false);
            rawdev::merged::compute(&fresh, &cancel, &|_, _| {}, &mut |_, _| {})?;
            let want = rawdev::merged::read_preview(&fresh.key).ok_or("fresh merge not readable")?;
            while rawdev::merged::cache_busy() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            rawdev::merged::forget_file(fresh.file);
            let differs = |a: &[[f32; 3]], b: &[[f32; 3]]| a.len() != b.len() || pixels_differ(a, b);
            if let Some(have) = rawdev::merged::read_preview(&key) {
                if differs(&have.px, &want.px) {
                    return Err(format!("the cached preview {key} of the batch is not the merge of its recipe ({})", recipe.settings));
                }
            }
        }
        // ⌘Z of the run (when its step is still the newest one of the library)
        if self.step_alive && matches!(self.lab.a.lib.manage.undo.last(), Some(library_manage::Undo::MergeBatch { .. })) {
            let mut a = Vec::new();
            self.lab.a.save_current();
            self.lab.a.lib.manage_undo();
            self.lab.frame();
            if matches!(self.lab.a.lib.manage.ask, Some(library_manage::Ask::RemoveMerge { undo: true, .. })) {
                self.lab.answer_ask(true);
            }
            self.lab.a.library_actions(std::mem::take(&mut a));
            self.undo_asked = true;
            self.step(990, BAct::JobsFinish)?;
            self.step(991, BAct::Wait(7))?;
            if let Some(f) = self.run.iter().find(|f| self.present.iter().any(|p| p.0 == **f)) {
                return Err(format!("⌘Z after the batch left the merged photo {f}"));
            }
        }
        Ok(())
    }
}

/// Run one batch sequence; `end`: a check instead of the usual end of a sequence.
fn brun(acts: &[BAct], verbose: bool, end: Option<&dyn Fn(&mut Batch) -> Check>) -> Result<(), (usize, String, Vec<String>)> {
    let mut b = Batch::new(verbose);
    let mut failed = None;
    for (n, act) in acts.iter().enumerate() {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| b.step(n, *act))) {
            Ok(Ok(())) => {}
            Ok(Err(why)) => {
                failed = Some((n, why));
                break;
            }
            Err(p) => {
                let why = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                failed = Some((n, format!("panic: {why}")));
                break;
            }
        }
    }
    if failed.is_none() {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match end {
            Some(f) => f(&mut b),
            None => b.finish(),
        })) {
            Ok(Ok(())) => {}
            Ok(Err(why)) => failed = Some((acts.len(), why)),
            Err(p) => failed = Some((acts.len(), format!("panic: {}", p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()))),
        }
    }
    let trace = std::mem::take(&mut b.lab.trace);
    PANICS.store(0, Ordering::SeqCst);
    crate::hdr_ui::cancel_all();
    b.lab.close();
    match failed {
        None => Ok(()),
        Some((n, why)) => Err((n, why, trace)),
    }
}

fn bshorten(acts: &[BAct], why: &str) -> Vec<BAct> {
    let class = |w: &str| w.chars().filter(|c| !c.is_ascii_digit()).take(40).collect::<String>();
    let mut best = acts.to_vec();
    let (mut tries, mut i) = (0, best.len());
    while i > 1 && tries < 25 {
        i -= 1;
        let mut shorter = best.clone();
        shorter.remove(i);
        tries += 1;
        if let Err((n, w, _)) = brun(&shorter, false, None) {
            if class(&w) == class(why) {
                shorter.truncate((n + 1).min(shorter.len()));
                best = shorter;
                i = i.min(best.len());
            }
        }
    }
    best
}

/// Random sequences with the batch "Alle Reihen zusammenfügen" (see the module docs for the switches).
#[test]
fn random_batch_sequences_keep_the_states_together() {
    let spec = std::env::var("GRAINEO_HDR_STRESS_BATCH").unwrap_or_else(|_| "10x25".into());
    let (count, rest) = spec.split_once('x').unwrap_or(("10", "25"));
    let (steps, first) = rest.split_once('@').unwrap_or((rest, "1"));
    let (count, steps, first): (u64, usize, u64) = (count.parse().unwrap(), steps.parse().unwrap(), first.parse().unwrap());
    let one: Option<u64> = std::env::var("GRAINEO_HDR_STRESS_BATCH_SEED").ok().and_then(|s| s.parse().ok());
    with_lab_lock(|| {
        let seeds: Vec<u64> = one.map_or_else(|| (first..first + count).collect(), |s| vec![s]);
        let t = std::time::Instant::now();
        let mut failures = Vec::new();
        for seed in &seeds {
            let acts = bsequence(*seed, steps);
            if let Err((n, why, trace)) = brun(&acts, one.is_some(), None) {
                eprintln!("\nHDR BATCH STRESS seed {seed} ({steps} steps) failed at step {n}: {why}");
                for l in &trace {
                    eprintln!("  {l}");
                }
                let short = if std::env::var_os("GRAINEO_HDR_STRESS_NO_SHORTEN").is_some() { acts[..(n + 1).min(acts.len())].to_vec() } else { bshorten(&acts[..(n + 1).min(acts.len())], &why) };
                eprintln!("  shortest failing sequence found ({} steps): {short:?}", short.len());
                failures.push((*seed, why));
                if failures.len() >= 5 {
                    break;
                }
            }
        }
        eprintln!("hdr batch stress: {} sequences × {steps} steps in {:.1} s, {} failed", seeds.len(), t.elapsed().as_secs_f32(), failures.len());
        assert!(failures.is_empty(), "failing seeds: {:?}", failures);
    });
}

fn bpasses(acts: &[BAct], end: Option<&dyn Fn(&mut Batch) -> Check>) {
    with_lab_lock(|| {
        if let Err((n, why, trace)) = brun(acts, false, end) {
            panic!("step {n}: {why}\n{}", trace.join("\n"));
        }
    });
}

/// H2 of the second audit: "Abbrechen" of a batch while a waiting merged photo of it is open and was worked on. The
/// photo stays with its area; the photos nobody touched leave; ⌘Z of the batch then asks, takes the rest out and one
/// more ⌘Z brings the worked one back.
#[test]
fn cancelling_a_batch_keeps_the_photo_that_was_worked_on() {
    use BAct::*;
    bpasses(&[Start(7), JobStep(0), Open(2), Paint(5), Wait(7), CancelAll, JobsFinish, Wait(70)], Some(&|b| {
        let (first, worked) = (b.run[0], b.run[2]);
        if !b.present.iter().any(|p| p.0 == worked) {
            return Err("the photo that was worked on left with the batch".into());
        }
        if !b.lab.has_work(worked) {
            return Err("its area is gone".into());
        }
        if b.present.iter().any(|p| p.0 == b.run[1]) {
            return Err("a waiting photo nobody touched stayed after the cancel".into());
        }
        let _ = first;
        // ⌘Z: asked about the work, then everything of the run is out
        b.step(100, Undo(false))?;
        if !b.present.iter().any(|p| p.0 == worked) {
            return Err("⌘Z removed the worked photo although the question was cancelled".into());
        }
        b.step(101, Undo(true))?;
        b.step(102, JobsFinish)?;
        if !b.present.is_empty() {
            return Err(format!("⌘Z of the batch left {:?}", b.present));
        }
        // its work is not lost: one more ⌘Z brings it back
        b.step(103, Undo(true))?;
        b.step(104, JobsFinish)?;
        if !b.present.iter().any(|p| p.0 == worked) || !b.lab.has_work(worked) {
            return Err("one more ⌘Z did not bring the worked photo back".into());
        }
        Ok(())
    }));
}

/// M5/B2 for the batch: ⌘Z while the merge of a bracket is past its last check (its result is being stored) and
/// while another one shows its preview already. Before, the step was used up and those photos stayed without a ⌘Z.
#[test]
fn undo_of_a_running_batch_takes_every_photo_of_it() {
    use BAct::*;
    for steps in [1usize, 2, 3, 4, 5, 6] {
        let mut acts = vec![Start(7)];
        acts.extend((0..steps).map(|_| JobStep(0)));
        acts.extend([Undo(true), JobsFinish, Wait(7)]);
        bpasses(&acts, Some(&|b| {
            if !b.present.is_empty() {
                return Err(format!("⌘Z of the running batch ({steps} points passed) left {:?}", b.present));
            }
            if b.batch_steps() != 0 {
                return Err("the ⌘Z step of the batch is still there".into());
            }
            Ok(())
        }));
    }
}

/// M4/B13: a second batch while one runs is refused (dialog and library), the first keeps its progress and its ⌘Z.
#[test]
fn a_second_batch_is_refused_while_one_runs() {
    use BAct::*;
    bpasses(&[Start(3), StartAgain, JobStep(0), StartAgain, Start(4), JobsFinish, Wait(70)], Some(&|b| {
        if b.present.len() != 2 || b.batch_steps() != 1 {
            return Err(format!("{} merged photos, {} steps of a batch", b.present.len(), b.batch_steps()));
        }
        Ok(())
    }));
}

/// B1: a change of another photo made while a batch runs waits behind the bracket being computed (and says so), is
/// computed before the next bracket of the batch, and "Abbrechen" takes a waiting job out at once.
#[test]
fn a_change_does_not_starve_behind_a_batch() {
    use crate::hdr_ui::gate;
    use BAct::*;
    for cancel in [false, true] {
        // the first bracket is done and opened, the second one is at its preview; then a change of the first photo
        bpasses(&[Start(7), JobStep(0), JobStep(0), JobStep(0), JobStep(0), Open(0), JobStep(0), Ghost(2)], Some(&|b| {
            let first = b.run[0];
            // (opening the photo may have started a job of its own: the change is the newest one, older ones end)
            let mut changes: Vec<gate::Job> = gate::jobs().into_iter().filter(|j| j.update && j.file == first).collect();
            let change = changes.pop().ok_or("the change started no job")?;
            for older in changes {
                gate::finish(older.id);
            }
            b.lab.settle();
            gate::step(change.id);
            b.lab.settle();
            if !crate::hdr_ui::waits(first) {
                return Err(format!("the change does not wait for its turn (jobs {:?})", gate::jobs()));
            }
            let line = b.lab.status();
            if !line.starts_with("Wartet") {
                return Err(format!("the waiting change shows \"{line}\""));
            }
            if cancel {
                // "Abbrechen" in the tool: out of the queue at once, although the bracket in front still computes
                b.lab.click("cancel", None);
                b.lab.settle();
                if crate::hdr_ui::waits(first) {
                    return Err("the cancelled change still waits for its turn".into());
                }
                // (the test holds every job at its end point)
                gate::finish(change.id);
                b.lab.settle();
                if crate::hdr_ui::job_alive(first) {
                    return Err("the cancelled change is still alive".into());
                }
                if !gate::jobs().iter().any(|j| !j.update) {
                    return Err("the bracket in front is not computing any more".into());
                }
                let line = b.lab.status();
                if line.starts_with("Wartet") || line.starts_with("Wird") {
                    return Err(format!("after the cancel the tool says \"{line}\""));
                }
                b.step(110, JobsFinish)?;
                return b.step(111, Wait(70));
            }
            // the bracket being computed ends: the change is computed now, the third bracket waits behind it
            let second = gate::jobs().into_iter().find(|j| !j.update).ok_or("the batch is not computing")?;
            gate::finish(second.id);
            b.lab.settle();
            if !rawdev::merged::jobs().iter().any(|j| j.file == first) {
                return Err(format!("after the bracket the change is not computed: computing {:?}, queue {:?}", rawdev::merged::jobs().iter().map(|j| j.file).collect::<Vec<_>>(), crate::hdr_ui::turn::queue()));
            }
            if let Some(third) = gate::jobs().into_iter().find(|j| !j.update) {
                gate::step(third.id);
                b.lab.settle();
                if rawdev::merged::jobs().iter().any(|j| j.file == third.file) {
                    return Err("the third bracket is computed beside the change".into());
                }
                if !crate::hdr_ui::waits(third.file) {
                    return Err("the third bracket does not wait behind the change".into());
                }
            }
            b.step(120, JobsFinish)?;
            b.step(121, Wait(70))
        }));
    }
}

/// B1: a job that waits for its turn leaves the queue at once when it is cancelled (it was stuck until the merge in
/// front of it ended; its photo kept saying "Wird zusammengefügt … 0 %").
#[test]
fn a_waiting_job_leaves_at_once_when_cancelled() {
    let _app_lock = crate::ui_test::app_lock();
    let flag = AtomicBool::new(false);
    let held = crate::hdr_ui::turn::take(4_713_001, &flag).expect("free");
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let t = std::thread::spawn(move || crate::hdr_ui::turn::take(4_713_002, &c2).is_some());
    while !crate::hdr_ui::turn::queue().contains(&4_713_002) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(rawdev::merged::busy_files().contains(&4_713_002), "a waiting job counts as busy (cache files are not removed under it)");
    cancel.store(true, Ordering::SeqCst);
    crate::hdr_ui::turn::wake();
    assert!(!t.join().unwrap(), "cancelled while waiting: no turn");
    assert!(!crate::hdr_ui::turn::queue().contains(&4_713_002) && !rawdev::merged::busy_files().contains(&4_713_002));
    // the order of arrival: the one who asked first goes first
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut threads = Vec::new();
    for k in 0..3i64 {
        let o = order.clone();
        threads.push(std::thread::spawn(move || {
            let f = AtomicBool::new(false);
            let turn = crate::hdr_ui::turn::take(4_713_010 + k, &f);
            o.lock().unwrap().push(k);
            drop(turn);
        }));
        while crate::hdr_ui::turn::queue().len() < k as usize + 1 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    drop(held);
    threads.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
    // a panic while holding the turn frees it
    let _ = std::thread::spawn(|| {
        let f = AtomicBool::new(false);
        let _turn = crate::hdr_ui::turn::take(4_713_020, &f);
        panic!("test");
    })
    .join();
    assert!(crate::hdr_ui::turn::take(4_713_021, &flag).is_some());
}

/// N2/B4: the app is quit while a batch runs. At the next start the photos nothing was computed for leave the
/// library again (their stacks go back), the ones with a result or with work stay.
#[test]
fn a_batch_ended_by_quitting_leaves_no_empty_photos() {
    use BAct::*;
    // the first bracket is computed, the second at its preview, the third waits and was worked on
    bpasses(&[Start(7), JobStep(0), JobStep(0), JobStep(0), JobStep(0), JobStep(0), Open(2), Paint(4), Wait(7), Kill, Wait(70)], Some(&|b| {
        let run = b.run.clone();
        let there: Vec<i64> = b.present.iter().map(|p| p.0).collect();
        if !there.contains(&run[0]) {
            return Err("the computed photo left".into());
        }
        if !there.contains(&run[2]) || !b.lab.has_work(run[2]) {
            return Err("the photo that was worked on left".into());
        }
        if there.contains(&run[1]) && !rawdev::merged::has_cached(run[1]) {
            return Err("an empty photo stayed".into());
        }
        Ok(())
    }));
    // nothing computed at all: every photo of the batch goes, the stacks are as before
    bpasses(&[Start(7), Kill, Wait(70)], Some(&|b| {
        if !b.present.is_empty() {
            return Err(format!("after the restart {:?} are left", b.present));
        }
        if b.user_stacks() != b.user_stacks {
            return Err(format!("{} stacks of the user are left, before the batch {}", b.user_stacks(), b.user_stacks));
        }
        Ok(())
    }));
}

/// The same for a single merge (N2): quit before anything was computed – gone at the next start; quit after the
/// preview – it stays and is computed when opened.
#[test]
fn a_single_merge_ended_by_quitting_is_settled_at_the_start() {
    use Act::*;
    passes_then(&[Create(0b0011100), Kill, Wait(70)], &|lab| {
        if let Some(f) = lab.hdr {
            let db = lab.db();
            return Err(format!("the merged photo nothing was computed for is still there: cached {}, preview {}, work {}, noted {:?}, rows loaded {}", rawdev::merged::has_cached(f), library::preview::has_any_developed(f), super::has_own_work(&db, f), library::merge::unfinished(&db.conn).iter().map(|u| u.file).collect::<Vec<_>>(), lab.a.lib.rows_loaded()));
        }
        Ok(())
    });
    passes(&[Create(0b0011100), JobStep(0), JobStep(0), Kill, JobsFinish, Wait(70), JobsFinish, Wait(70)]);
}

/// N4/B5: a merged photo whose result is not in the cache opens with its reference frame standing in. That picture is
/// never stored or handed out as a picture of the merged photo: the renderers of previews refuse it, a copy for the
/// clipboard merges first.
#[test]
fn the_reference_standing_in_is_never_stored_as_the_merged_photo() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70), Photo(0), Wait(7), ClearCache], &|lab| {
        let path = lab.hdr_path.clone().ok_or("no merged photo")?;
        let file = lab.hdr.ok_or("no merged photo")?;
        rawdev::merged::forget_file(file);
        let s = crate::Settings::default();
        let doc = rawdev::RawDoc::open(&path, &|| true)?.ok_or("not opened")?;
        if !doc.stands_in() {
            return Err("the photo does not stand in with its reference".into());
        }
        // large preview, repair of previews, the document itself
        for (what, r) in [("large preview", crate::large_preview::render(&path, &s, 512)), ("preview repair", crate::preview_repair::standard(&path, 512)), ("document", crate::large_preview::render_doc(&doc, &s, 512))] {
            match r {
                Err(e) if e == rawdev::doc::MERGE_NOT_READY => {}
                Err(e) => return Err(format!("{what}: {e}")),
                Ok(_) => return Err(format!("{what}: the reference frame was rendered as the merged photo")),
            }
        }
        if !cache_keys(file).0.is_empty() {
            return Err("a preview renderer computed the merge".into());
        }
        // a copy is an output: merged first (through the one entrance for merges), then rendered
        crate::hdr_ui::gate::set(false);
        let img = crate::photo_copy::develop_one(&path, &s, true);
        crate::hdr_ui::gate::set(true);
        img.map_err(|e| format!("copy: {e}"))?;
        if cache_keys(file).0.is_empty() {
            return Err("the copy was rendered without the merge".into());
        }
        let doc = rawdev::RawDoc::open(&path, &|| true)?.ok_or("not opened")?;
        if doc.stands_in() {
            return Err("after the copy the photo still stands in".into());
        }
        Ok(())
    });
}

/// B6: an ordinary photo that comes into the library after a merged photo was removed never takes its id; ⌘Z brings
/// the merged photo back.
#[test]
fn a_new_photo_does_not_take_the_place_of_a_removed_merge() {
    use Act::*;
    passes(&[Create(0b0011100), JobsFinish, Wait(70), Paint(3), JobsFinish, Wait(70), Remove(true), Wait(7), NewPhoto, Wait(7), Undo, JobsFinish, Photo(7), Wait(70), NewPhoto, Wait(70)]);
}

/// B8: a panic inside a merge is a failed merge – nothing stays locked: the job is not alive, the photo not "being
/// created", the title bar shows no merge, the next merge takes its turn; a batch goes on with its next bracket.
#[test]
fn a_panic_in_a_merge_locks_nothing() {
    use crate::hdr_ui::gate;
    // a single merge: its first computation panics → the photo leaves again (nothing of it was shown), all is free
    passes_then(&[Act::Create(0b0011100)], &|lab| {
        let file = lab.hdr.ok_or("no merged photo")?;
        gate::panic_at(gate::Point::Preview);
        lab.may_vanish = true;
        lab.finish_jobs();
        lab.settle();
        if crate::hdr_ui::job_alive(file) || crate::hdr_ui::creating(file) || !rawdev::merged::jobs().is_empty() || !crate::hdr_ui::turn::queue().is_empty() {
            return Err(format!("after the panic: alive {}, creating {}, computing {}, queue {:?}", crate::hdr_ui::job_alive(file), crate::hdr_ui::creating(file), rawdev::merged::jobs().len(), crate::hdr_ui::turn::queue()));
        }
        lab.step(100, Act::Wait(7))?;
        // the next merge is computed as usual
        lab.step(101, Act::Create(0b0011100))?;
        lab.step(102, Act::JobsFinish)?;
        lab.step(103, Act::Wait(70))?;
        if !lab.status().starts_with("Aktuell") {
            return Err(format!("the merge after the panic says \"{}\"", lab.status()));
        }
        Ok(())
    });
    // a batch: the bracket that panics fails, the others are computed, the batch ends
    bpasses(&[BAct::Start(7)], Some(&|b| {
        gate::panic_at(gate::Point::Full);
        b.stop_asked = true;
        b.step(100, BAct::JobsFinish)?;
        b.step(101, BAct::Wait(70))?;
        if crate::hdr_batch::active() {
            return Err("the batch still counts as running".into());
        }
        let done = b.present.iter().filter(|p| p.1).count();
        if done < 2 {
            return Err(format!("{done} brackets were computed after the panic of one"));
        }
        Ok(())
    }));
}

/// Seed 58 (third run): ⌘Z while the first computation ran (the step waits for the job), then "HDR entfernen", then ⌘Z
/// brought the photo back – and the ⌘Z that had waited was carried out when the job ended: the photo left again.
#[test]
fn a_waiting_undo_ends_when_the_photo_is_removed() {
    use Act::*;
    passes(&[Create(30), Undo, Remove(false), JobStep(2), Undo, JobStep(0), Wait(7), JobsFinish, Photo(7), Wait(70)]);
}

/// B11: ⌘Z while the first computation runs waits for that job; a change that arrives before the job ends overtakes
/// it (the job ends without a word). The ⌘Z is carried out then – it does not stay waiting (and fire at some later
/// failure of the photo).
#[test]
fn a_waiting_undo_is_carried_out_when_its_job_is_overtaken() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobStep(0), Undo, Ghost(0), JobsFinish, Wait(7), JobsFinish, Wait(7)], &|lab| {
        if !lab.a.lib.manage.merge_undo_waiting.is_empty() {
            return Err(format!("a ⌘Z still waits: {:?}", lab.a.lib.manage.merge_undo_waiting));
        }
        if lab.hdr.is_some() {
            return Err("the photo ⌘Z asked to take back is still there".into());
        }
        Ok(())
    });
}

/// B9: the results of a batch get their automatic tone when they are computed, not when they are opened first – the
/// tile and an export from the grid show what Develop shows; opening changes nothing.
#[test]
fn batch_results_get_their_automatic_tone_unseen() {
    use BAct::*;
    bpasses(&[Start(3), JobsFinish, Wait(70)], Some(&|b| {
        let mut applied = Vec::new();
        for p in b.present.clone() {
            let db = b.lab.db();
            let r = library::merge::recipe(&db.conn, p.0).ok_or("no recipe")?;
            let s = rawdev::merged::parse_settings(&r.settings)?;
            let Some(v) = s.auto_tone.and_then(|a| a.applied) else { return Err(format!("the merged photo {} has no automatic tone yet", p.0)) };
            let stored = library::merge::work(&db.conn, p.0).develop.first().and_then(|j| serde_json::from_str::<crate::Settings>(j).ok()).ok_or("no develop settings stored")?;
            if crate::hdr_auto::tone_of(&stored) != v {
                return Err(format!("stored sliders {:?}, the automatic set {v:?}", crate::hdr_auto::tone_of(&stored)));
            }
            if v == [0.0; 5] {
                return Err("the automatic set nothing".into());
            }
            if p.2 {
                return Err("the automatic tone counts as work of the user".into());
            }
            applied.push(v);
        }
        // opening the photo changes nothing
        b.step(100, Open(0))?;
        b.step(101, JobsFinish)?;
        b.step(102, Wait(70))?;
        b.step(103, JobsFinish)?;
        let i = b.lab.hdr_entry().ok_or("not shown")?;
        let shown = crate::hdr_auto::tone_of(&b.lab.a.entries[i].settings);
        if shown != applied[0] {
            return Err(format!("opened: sliders {shown:?}, set after the batch {:?}", applied[0]));
        }
        Ok(())
    }));
}

/// H3: a cached result whose full resolution does not read (damaged file) is computed again when it is needed; before,
/// it counted as done and zooming in failed for good.
#[test]
fn a_damaged_result_is_computed_again() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70), Photo(0), Wait(7)], &|lab| {
        let (path, key) = (lab.hdr_path.clone().ok_or("no merged photo")?, lab.recipe().ok_or("no recipe")?.1);
        std::fs::write(rawdev::merged::full_result_path(&key), b"damaged").map_err(|e| e.to_string())?;
        if rawdev::merged::read_full(&key).is_some() {
            return Err("the damaged file reads".into());
        }
        crate::hdr_ui::gate::set(false);
        let r = rawdev::RawDoc::open(&path, &|| true).and_then(|d| d.ok_or_else(|| "not opened".to_string())).and_then(|d| d.render_full(&crate::Settings::default()).map(|_| ()));
        crate::hdr_ui::gate::set(true);
        r.map_err(|e| format!("full resolution of the damaged result: {e}"))?;
        if rawdev::merged::read_full(&key).is_none() {
            return Err("the result was not computed again".into());
        }
        Ok(())
    });
}

/// Tests of the fourth audit that show open findings (ignored).
#[path = "hdr_audit4_test.rs"]
mod audit4;
