//! Choice of the model of the semantic search in stages, and reading the photos in again for a new
//! model.
//!
//! - **Standard** (`clip-b32`): CLIP ViT-B/32 image vectors (tables `image_vector`/`text_vector`, also from the
//!   imported ones) with the small multilingual text encoder.
//! - **Mehrsprachig** (`mclip`): the same image vectors with the larger M-CLIP text model (`xlmr.rs`);
//!   no reading in. Both CLIP stages also set the text-model setting of `semantic` (`textModel`).
//! - **Beste Treffer** (`siglip2-base`, SigLIP 2, `siglip.rs`); `siglip2-large` exists as a hidden
//!   stage (setting only, not offered in the settings window). SigLIP vectors live per model in
//!   `image_vector_model` / `text_vector_model` (column `model` = repo name), so switching never loses
//!   vectors and switching back is immediate.
//!
//! Switching to a stage whose vectors are not complete sets it as **target**: a background job
//! (QoS BACKGROUND, throttled, cancellable) computes the missing image and description vectors; the
//! search keeps using the **active** stage until the job is through, then the target becomes active.
//! Settings keys: `searchModel` (active), `searchModelTarget` (being prepared).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::db::Db;
use crate::module::Definition;
use crate::similar::Vectors;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Fast,
    Multilingual,
    Balanced,
    Best,
}

/// Stages offered in the settings window (one card each).
pub const STAGES: [Stage; 3] = [Stage::Fast, Stage::Multilingual, Stage::Balanced];
/// All stages including the hidden SigLIP 2 Large.
pub const ALL_STAGES: [Stage; 4] = [Stage::Fast, Stage::Multilingual, Stage::Balanced, Stage::Best];

/// Facts of a stage for the choice in the settings. German texts (translated where displayed).
/// Measured values: M1 Pro, graineo's own implementation.
pub struct Info {
    /// short name of the stage
    pub title: &'static str,
    /// purpose in one sentence
    pub purpose: &'static str,
    pub strengths: &'static str,
    pub weaknesses: &'static str,
    /// recommendation: for whom the stage fits
    pub for_whom: &'static str,
    pub languages: &'static str,
    pub maker: &'static str,
    pub license: &'static str,
    /// model name(s) as on Hugging Face
    pub model: &'static str,
    /// working memory while searching (text part loaded), MB
    pub memory_search_mb: u32,
    /// working memory while reading photos (image part loaded), MB
    pub memory_read_mb: u32,
    /// one search query, ms
    pub ms_query: u32,
    /// one photo at full speed incl. reading its preview, ms
    pub ms_photo: u32,
    /// share of searched photos found at rank 1 for German / English queries (XTD10, %), own measurement
    pub r1_de: f32,
    pub r1_en: f32,
    /// quality on a scale 1…5 (from `r1_de`)
    pub quality: u8,
    /// width of the vectors
    pub dim: usize,
}

impl Stage {
    /// Value in the settings.
    pub fn key(self) -> &'static str {
        match self {
            Stage::Fast => "clip-b32",
            Stage::Multilingual => "mclip",
            Stage::Balanced => "siglip2-base",
            Stage::Best => "siglip2-large",
        }
    }

    pub fn from_key(k: &str) -> Option<Stage> {
        ALL_STAGES.into_iter().find(|s| s.key() == k)
    }

    /// Module(s) the stage needs.
    pub fn modules(self) -> &'static [&'static Definition] {
        match self {
            Stage::Fast => &[&crate::module::TEXT, &crate::module::IMAGE],
            Stage::Multilingual => &[&crate::module::MCLIP, &crate::module::IMAGE],
            Stage::Balanced => &[&crate::module::SIGLIP_BASE],
            Stage::Best => &[&crate::module::SIGLIP_LARGE],
        }
    }

    /// SigLIP module (its repo name is the value of the `model` column); `None` for the CLIP stage.
    pub fn siglip(self) -> Option<&'static Definition> {
        match self {
            Stage::Fast | Stage::Multilingual => None,
            Stage::Balanced => Some(&crate::module::SIGLIP_BASE),
            Stage::Best => Some(&crate::module::SIGLIP_LARGE),
        }
    }

    /// Model folder for queries (installed module, for tests also an existing Hugging Face cache).
    pub fn folder(self) -> Option<PathBuf> {
        match self {
            Stage::Fast => crate::semantic::text_folder(),
            Stage::Multilingual => crate::semantic::mclip_folder(),
            _ => self.siglip().and_then(|d| crate::siglip::folder(d.id, d.repo)),
        }
    }

    /// Text model of a CLIP stage (`None` for SigLIP).
    pub fn text_model(self) -> Option<crate::semantic::TextModel> {
        match self {
            Stage::Fast => Some(crate::semantic::TextModel::Standard),
            Stage::Multilingual => Some(crate::semantic::TextModel::Multilingual),
            _ => None,
        }
    }

    /// Does choosing this stage mean reading all photos in again (own image vectors)?
    pub fn needs_reindex(self) -> bool {
        self.siglip().is_some()
    }

    /// Download size of the stage's own files (the CLIP image module the "Mehrsprachig" stage shares with
    /// "Standard" is counted only there).
    pub fn download_bytes(self) -> u64 {
        let own = |d: &&&Definition| self == Stage::Fast || d.id != crate::module::IMAGE.id;
        self.modules().iter().filter(own).flat_map(|d| d.files.iter()).map(|f| f.size).sum()
    }

    pub fn info(self) -> &'static Info {
        match self {
            Stage::Fast => &FAST,
            Stage::Multilingual => &MULTILINGUAL,
            Stage::Balanced => &BALANCED,
            Stage::Best => &BEST,
        }
    }
}

pub static FAST: Info = Info {
    title: "Standard",
    purpose: "Findet Fotos nach Inhalt mit wenig Rechenzeit und Speicher.",
    strengths: "Sofort bereit, liest neue Fotos schnell ein, braucht wenig Speicher.",
    weaknesses: "Versteht Deutsch und andere Sprachen schwächer als Englisch, übersieht öfter Einzelheiten.",
    for_whom: "Für ältere oder knappe Macs und wenn Suchwörter meist englisch sind.",
    languages: "50+ Sprachen (Englisch am besten)",
    maker: "OpenAI (Bild), sentence-transformers (Text)",
    license: "MIT / Apache-2.0",
    model: "CLIP ViT-B/32 + clip-ViT-B-32-multilingual-v1",
    memory_search_mb: MEASURED_FAST.0,
    memory_read_mb: MEASURED_FAST.1,
    ms_query: MEASURED_FAST.2,
    ms_photo: MEASURED_FAST.3,
    r1_de: MEASURED_FAST.4,
    r1_en: MEASURED_FAST.5,
    quality: 2,
    dim: 512,
};

pub static MULTILINGUAL: Info = Info {
    title: "Mehrsprachig",
    purpose: "Versteht Suchwörter auf Deutsch und in vielen anderen Sprachen deutlich besser; die Fotos bleiben eingelesen.",
    strengths: "Kein Neu-Einlesen: wirkt sofort nach dem Installieren, auf allen schon eingelesenen Fotos.",
    weaknesses: "Größerer Download, jede Anfrage dauert etwas länger; sieht Fotos so wie „Standard“.",
    for_whom: "Für alle, die auf Deutsch suchen und nicht alle Fotos neu einlesen wollen.",
    languages: "48 Trainingssprachen, weitere über XLM-RoBERTa (100 Sprachen)",
    maker: "F. Carlsson et al. (Multilingual-CLIP), OpenAI (Bild)",
    license: "MIT",
    model: "M-CLIP/XLM-Roberta-Large-Vit-B-32 + CLIP ViT-B/32",
    memory_search_mb: crate::module::MCLIP_MEMORY_MB,
    memory_read_mb: MEASURED_FAST.1,
    ms_query: crate::module::MCLIP_MS,
    ms_photo: MEASURED_FAST.3,
    r1_de: MEASURED_MCLIP.0,
    r1_en: MEASURED_MCLIP.1,
    quality: 3,
    dim: 512,
};

pub static BALANCED: Info = Info {
    title: "Beste Treffer",
    purpose: "Die besten Treffer: liest jedes Foto mit einem neuen, genaueren Bildmodell ein.",
    strengths: "Versteht Suchwörter in vielen Sprachen am besten und sieht das ganze Foto statt nur der Mitte.",
    weaknesses: "Alle Fotos werden einmal neu eingelesen (im Hintergrund, gedrosselt); Japanisch schwächer.",
    for_whom: "Für die meisten: wer oft nach Inhalten sucht und einmal Einlesezeit in Kauf nimmt.",
    languages: "109 Sprachen im Training (u. a. Deutsch, Englisch, Französisch, Spanisch, Japanisch)",
    maker: "Google (SigLIP 2 Base)",
    license: "Apache-2.0",
    model: "google/siglip2-base-patch16-256",
    memory_search_mb: MEASURED_BALANCED.0,
    memory_read_mb: MEASURED_BALANCED.1,
    ms_query: MEASURED_BALANCED.2,
    ms_photo: MEASURED_BALANCED.3,
    r1_de: MEASURED_BALANCED.4,
    r1_en: MEASURED_BALANCED.5,
    quality: 4,
    dim: 768,
};

pub static BEST: Info = Info {
    title: "Beste Treffer – groß",
    purpose: "Die genauesten Treffer, wenn Speicher und Einlesezeit keine Rolle spielen.",
    strengths: "Erkennt auch Einzelheiten und ungewöhnliche Beschreibungen am sichersten.",
    weaknesses: "Großer Download, viel Arbeitsspeicher, das Einlesen aller Fotos dauert Stunden.",
    for_whom: "Für Macs mit viel Arbeitsspeicher (32 GB und mehr).",
    languages: "109 Sprachen im Training (u. a. Deutsch, Englisch, Französisch, Spanisch, Japanisch)",
    maker: "Google (SigLIP 2 Large)",
    license: "Apache-2.0",
    model: "google/siglip2-large-patch16-256",
    memory_search_mb: MEASURED_BEST.0,
    memory_read_mb: MEASURED_BEST.1,
    ms_query: MEASURED_BEST.2,
    ms_photo: MEASURED_BEST.3,
    r1_de: MEASURED_BEST.4,
    r1_en: MEASURED_BEST.5,
    quality: 5,
    dim: 1024,
};

// measured (memory search MB, memory read MB, ms per query, ms per photo, R@1 de, R@1 en)
// M1 Pro, 16 GB, while other builds were running (load 15–70, swap full): times are upper bounds.
// Quality: XTD10 with the own Rust implementation.
pub const MEASURED_FAST: (u32, u32, u32, u32, f32, f32) = (crate::module::STD_MEMORY_MB, 650, crate::module::STD_MS, 60, 39.2, 47.9);
/// R@1 de/en of M-CLIP text with CLIP B/32 images
/// (English: PyTorch measurement of the same model, messung-textmodell.md)
const MEASURED_MCLIP: (f32, f32) = (51.9, 53.7);
pub const MEASURED_BALANCED: (u32, u32, u32, u32, f32, f32) = (410, 480, 70, 270, 62.1, 77.2);
/// Large: memory measured (footprint 1,3 GB while reading); times only estimated from the arithmetic
/// (about 3,5× Base) because the measurement ran under heavy load with swapping (2,8 s per photo).
pub const MEASURED_BEST: (u32, u32, u32, u32, f32, f32) = (1300, 1300, 250, 950, 66.3, 77.6);
pub const LARGE_QUALITY: &str = "Top-1 66,3 %, Top-10 93,8 % (Deutsch)";

/// Recommended stage ("Für die meisten").
pub const RECOMMENDED: Stage = Stage::Balanced;

// ───────────────────────────────────────────── Choice

static ROUND: AtomicU64 = AtomicU64::new(0);

/// Changes whenever the active stage or the target changes (a loaded search state must be rebuilt).
pub fn round() -> u64 {
    ROUND.load(Ordering::Relaxed)
}

/// (active, target, model of the active stage present) as last read; the settings file and the model
/// folder are read at most once a second (the settings window and the search ask every frame).
static CHOICE: Mutex<Option<(Instant, Stage, Option<Stage>)>> = Mutex::new(None);

fn choice() -> (Stage, Option<Stage>) {
    let mut c = CHOICE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, a, z)) = *c {
        if t.elapsed() < Duration::from_secs(1) {
            return (a, z);
        }
    }
    // the CLIP stages follow the text-model choice of `semantic` (M-CLIP as soon as it is installed,
    // unless "standard" was chosen); a SigLIP stage whose model is missing falls back to them
    let clip = match crate::semantic::selected_text_model() {
        crate::semantic::TextModel::Multilingual => Stage::Multilingual,
        crate::semantic::TextModel::Standard => Stage::Fast,
    };
    let a = match crate::settings::value("searchModel").as_str().and_then(Stage::from_key) {
        Some(s) if s.siglip().is_some() && s.folder().is_some() => s,
        _ => clip,
    };
    let z = crate::settings::value("searchModelTarget").as_str().and_then(Stage::from_key).filter(|s| *s != a);
    *c = Some((Instant::now(), a, z));
    (a, z)
}

fn forget_choice() {
    *CHOICE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    ROUND.fetch_add(1, Ordering::Relaxed);
}

/// Stage the search uses now. A SigLIP stage whose model is missing falls back to "Schnell".
pub fn active() -> Stage {
    choice().0
}

/// Stage being prepared (vectors computed in the background), if any.
pub fn target() -> Option<Stage> {
    choice().1
}

/// A module was installed or removed: read the choice again.
pub fn modules_changed() {
    forget_choice();
}

fn set_active(s: Stage) {
    if let Some(m) = s.text_model() {
        crate::semantic::set_text_model(m);
    }
    crate::settings::set("searchModel", serde_json::json!(s.key()));
    crate::settings::set("searchModelTarget", serde_json::Value::Null);
    forget_choice();
}

/// The user chooses a stage. "Schnell" (and a stage whose vectors are complete) becomes active at
/// once; otherwise it becomes the target and the job reads the photos in. Returns: active right away.
pub fn choose(db: &Db, s: Stage) -> bool {
    stop();
    if !s.needs_reindex() || progress_of(db, s).is_some_and(|(done, total)| done >= total) {
        set_active(s);
        return true;
    }
    crate::settings::set("searchModelTarget", serde_json::json!(s.key()));
    forget_choice();
    false
}

/// Give up the target (the vectors computed so far stay for later).
pub fn cancel_target() {
    stop();
    crate::settings::set("searchModelTarget", serde_json::Value::Null);
    forget_choice();
}

// ───────────────────────────────────────────── Vectors

/// Files that get a vector (as for the CLIP vectors: present, no videos).
const ELIGIBLE: &str = "d.status = 0 AND d.format <> 'VIDEO'";

/// Image vectors of a stage: done and total (photos). `None` if the library cannot be read.
pub fn progress_of(db: &Db, s: Stage) -> Option<(i64, i64)> {
    let n = |sql: &str, p: &[&dyn rusqlite::ToSql]| db.conn.query_row(sql, p, |r| r.get::<_, i64>(0)).ok();
    let total = n(&format!("SELECT count(*) FROM file d WHERE {ELIGIBLE}"), &[])?;
    let done = match s.siglip() {
        None => n(&format!("SELECT count(*) FROM file d WHERE {ELIGIBLE} AND d.id IN (SELECT file FROM image_vector)"), &[])?,
        Some(d) => n(&format!("SELECT count(*) FROM file d WHERE {ELIGIBLE} AND d.id IN (SELECT file FROM image_vector_model WHERE model = ?1)"), &[&d.repo])?,
    };
    Some((done, total))
}

fn blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn load_table(db: &Db, table: &str, model: &str, dim: usize) -> Result<Vectors, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let mut v = Vectors::with_dim(dim);
    let mut st = db.conn.prepare(&format!("SELECT file, vec FROM {table} WHERE model = ?1")).map_err(f)?;
    let mut rows = st.query([model]).map_err(f)?;
    let mut buf = Vec::with_capacity(dim);
    while let Some(r) = rows.next().map_err(f)? {
        let (d, b): (i64, Vec<u8>) = (r.get(0).map_err(f)?, r.get(1).map_err(f)?);
        if b.len() == dim * 4 {
            buf.clear();
            buf.extend(b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
            v.insert_into(d, &buf);
        }
    }
    Ok(v)
}

/// Image and description vectors of a stage for the search.
pub fn load_vectors(db: &Db, s: Stage) -> Result<(Vectors, Option<Vectors>), String> {
    match s.siglip() {
        None => {
            let m = s.text_model().unwrap_or(crate::semantic::TextModel::Standard);
            Ok((Vectors::load(db)?, crate::similar::load_text_model(db, m).ok()))
        }
        Some(d) => {
            let dim = s.info().dim;
            Ok((load_table(db, "image_vector_model", d.repo, dim)?, load_table(db, "text_vector_model", d.repo, dim).ok()))
        }
    }
}

/// Encoder for search queries of a stage.
pub enum QueryEncoder {
    /// CLIP space: standard or M-CLIP text model
    Clip(crate::semantic::QueryEncoder),
    Siglip(crate::siglip::TextTower),
}

impl QueryEncoder {
    pub fn load(s: Stage) -> Result<QueryEncoder, String> {
        let dir = s.folder().ok_or("Suchmodell fehlt")?;
        Ok(match s.text_model() {
            Some(m) => QueryEncoder::Clip(crate::semantic::QueryEncoder::load(m)?),
            None => QueryEncoder::Siglip(crate::siglip::TextTower::load(&dir)?),
        })
    }

    /// Query → normalized vector (in the space of the stage's image vectors).
    pub fn embed(&self, text: &str) -> Vec<f32> {
        match self {
            QueryEncoder::Clip(e) => e.embed(text),
            QueryEncoder::Siglip(t) => t.embed_one(text).unwrap_or_else(|_| vec![0.0; t.dim()]),
        }
    }
}

// ───────────────────────────────────────────── Reading the photos in (background job)

/// State of the running job for the settings and the activity display.
#[derive(Clone, Debug)]
pub struct Progress {
    pub stage: Stage,
    /// done / all (photos, or descriptions in the second part)
    pub done: i64,
    pub total: i64,
    /// computed in this run and the time since its start (for the remaining time)
    pub run_done: i64,
    pub since: Instant,
    /// "Fotos" or "Beschreibungen"
    pub part: &'static str,
}

impl Progress {
    /// Remaining time from the pace of this run (including the pauses of the throttling).
    pub fn remaining(&self) -> Option<Duration> {
        if self.run_done < 8 {
            return None;
        }
        let per = self.since.elapsed().as_secs_f64() / self.run_done as f64;
        Some(Duration::from_secs_f64(per * (self.total - self.done).max(0) as f64))
    }
}

static PROGRESS: Mutex<Option<Progress>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn progress() -> Option<Progress> {
    PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

/// Ask a running job to stop (it ends after the current photos).
pub fn stop() {
    if running() {
        CANCEL.store(true, Ordering::Relaxed);
    }
}

/// Throttle: after every batch the job pauses `factor` times as long as it computed (1.0 = at most
/// about half of the time computes). `GRAINEO_REINDEX_FULL=1` (measurements) does not pause.
fn throttle_factor() -> f64 {
    if std::env::var_os("GRAINEO_REINDEX_FULL").is_some() {
        0.0
    } else {
        1.0
    }
}

/// Stage that needs vectors now: the target, else an active SigLIP stage (new photos).
pub fn due() -> Option<Stage> {
    target().or_else(|| Some(active()).filter(|s| s.siglip().is_some()))
}

/// Start the job in a background thread if a stage needs vectors and none runs. `on_change` is called
/// after the job made the target active. Returns: started.
pub fn start(on_change: impl Fn() + Send + 'static) -> bool {
    let Some(stage) = due() else { return false };
    if stage.folder().is_none() || RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    CANCEL.store(false, Ordering::Relaxed);
    std::thread::spawn(move || {
        crate::platform::background_priority();
        let r = Db::standard().map_err(|e| e.to_string()).and_then(|db| run(&db, stage, &CANCEL, &|p| *PROGRESS.lock().unwrap_or_else(|e| e.into_inner()) = Some(p)));
        *PROGRESS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        RUNNING.store(false, Ordering::SeqCst);
        match r {
            Ok(true) => {
                if target() == Some(stage) {
                    set_active(stage);
                    on_change();
                }
            }
            Ok(false) => {}
            Err(e) => eprintln!("search model {}: {e}", stage.key()),
        }
    });
    true
}

/// Compute all missing image vectors (then description vectors) of `stage`. Returns: complete
/// (`false` = cancelled). Photos whose preview cannot be read get a zero vector (not tried again;
/// the search skips zero vectors).
pub fn run(db: &Db, stage: Stage, cancel: &AtomicBool, report: &dyn Fn(Progress)) -> Result<bool, String> {
    let d = stage.siglip().ok_or("„Schnell“ liest über den bisherigen Weg ein")?;
    let dir = stage.folder().ok_or("Suchmodell fehlt")?;
    let factor = throttle_factor();
    let (mut done, total) = progress_of(db, stage).ok_or("Bibliothek nicht lesbar")?;
    let since = Instant::now();
    let mut run_done = 0i64;
    if done < total {
        let tower = crate::siglip::ImageTower::load(&dir)?;
        let dim = tower.dim();
        report(Progress { stage, done, total, run_done, since, part: "Fotos" });
        let sql = format!("SELECT d.id, s.path || o.path || d.name, d.format, coalesce(d.size, 0), coalesce(d.mtime, 0), d.orientation FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE {ELIGIBLE} AND d.id NOT IN (SELECT file FROM image_vector_model WHERE model = ?1) LIMIT 16");
        let use_cache = std::env::var_os("GRAINEO_REINDEX_NO_CACHE").is_none();
        let (from_cache, from_file) = (std::cell::Cell::new(0usize), std::cell::Cell::new(0usize));
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let list: Vec<(i64, PathBuf, String, i64, i64, Option<i64>)> = {
                let mut st = db.conn.prepare_cached(&sql).map_err(|e| e.to_string())?;
                let r = st
                    .query_map([d.repo], |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?), r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
                    .map_err(|e| e.to_string())?
                    .flatten()
                    .collect();
                r
            };
            if list.is_empty() {
                break;
            }
            for chunk in list.chunks(4) {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                let t = Instant::now();
                // first the app's thumbnails (no access to the original, e.g. on the NAS), the file only as fallback
                let imgs: Vec<Option<image::RgbImage>> = chunk
                    .iter()
                    .map(|(file, p, f, size, mtime, orient)| {
                        let key = crate::preview::cache_key_with_orientation(crate::preview::cache_key(&p.to_string_lossy(), *size, *mtime), *orient);
                        let hit = use_cache.then(|| cached_preview(*file, key)).flatten();
                        if hit.is_some() {
                            from_cache.set(from_cache.get() + 1);
                            return hit;
                        }
                        from_file.set(from_file.get() + 1);
                        crate::preview::load(p, f, 512).ok().map(|x| x.0)
                    })
                    .collect();
                let ok: Vec<&image::RgbImage> = imgs.iter().flatten().collect();
                let mut vecs = tower.embed(&ok).into_iter();
                let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
                for ((file, ..), img) in chunk.iter().zip(&imgs) {
                    let v = if img.is_some() { vecs.next().unwrap_or_else(|| vec![0.0; dim]) } else { vec![0.0; dim] };
                    tx.execute("INSERT OR REPLACE INTO image_vector_model(file, model, vec) VALUES(?1, ?2, ?3)", rusqlite::params![file, d.repo, blob(&v)]).map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())?;
                done += chunk.len() as i64;
                run_done += chunk.len() as i64;
                report(Progress { stage, done, total: total.max(done), run_done, since, part: "Fotos" });
                std::thread::sleep(t.elapsed().mul_f64(factor));
            }
        }
        eprintln!("search model {}: {} photos from the preview cache, {} from their files", stage.key(), from_cache.get(), from_file.get());
    }
    // description vectors of the AI descriptions (same model, text part)
    let open: Vec<(i64, String)> = {
        let mut st = db
            .conn
            .prepare("SELECT k.file, coalesce(k.keywords,'[]'), coalesce(k.scene,''), coalesce(k.reason,'') FROM ai k WHERE k.file NOT IN (SELECT file FROM text_vector_model WHERE model = ?1)")
            .map_err(|e| e.to_string())?;
        let r = st
            .query_map([d.repo], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|(f, k, s, b)| (f, description_text(&k, &s, &b)))
            .collect();
        r
    };
    if !open.is_empty() {
        let tower = crate::siglip::TextTower::load(&dir)?;
        let (total, since) = (open.len() as i64, Instant::now());
        for (i, chunk) in open.chunks(8).enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let t = Instant::now();
            let texts: Vec<&str> = chunk.iter().map(|x| x.1.as_str()).collect();
            let vecs = tower.embed(&texts)?;
            let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
            for ((file, _), v) in chunk.iter().zip(&vecs) {
                tx.execute("INSERT OR REPLACE INTO text_vector_model(file, model, vec) VALUES(?1, ?2, ?3)", rusqlite::params![file, d.repo, blob(v)]).map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            let n = ((i + 1) * 8).min(open.len()) as i64;
            report(Progress { stage, done: n, total, run_done: n, since, part: "Beschreibungen" });
            std::thread::sleep(t.elapsed().mul_f64(factor));
        }
    }
    Ok(true)
}

/// A thumbnail the app already has for this file, read only (the cache's inventory and files stay untouched,
/// nothing is written or removed): the developed preview first (what the photo looks like in graineo), then
/// the small grid levels. `None` if none is there or it cannot be decoded. 256 px are enough for SigLIP.
pub fn cached_preview(file: i64, key: u64) -> Option<image::RgbImage> {
    let decode = |p: PathBuf| -> Option<image::RgbImage> {
        let bytes = std::fs::read(p).ok()?;
        image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok().map(|i| i.to_rgb8())
    };
    let cache = crate::cache_folder();
    let developed = crate::preview::developed_folder();
    crate::preview::SMALL_CHOICES
        .iter()
        .find_map(|l| decode(developed.join(format!("{file}-{l}.jpg"))))
        .or_else(|| crate::preview::SMALL_CHOICES.iter().find_map(|l| decode(crate::preview::file_in(&cache, key, *l))))
}

/// Text of an AI description as it is embedded (keywords sorted, scene, reasoning).
pub fn description_text(keywords_json: &str, scene: &str, reason: &str) -> String {
    crate::semantic::description_text(keywords_json, scene, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_modules() {
        for s in STAGES {
            assert_eq!(Stage::from_key(s.key()), Some(s));
            assert!(!s.modules().is_empty());
            assert!(s.download_bytes() > 0);
            let i = s.info();
            assert!((1..=5).contains(&i.quality));
        }
        assert_eq!(Stage::Balanced.info().dim, 768);
        assert_eq!(Stage::Best.info().dim, 1024);
        assert!(Stage::Fast.siglip().is_none());
    }

    #[test]
    fn remaining_time_from_pace() {
        let p = Progress { stage: Stage::Balanced, done: 100, total: 1100, run_done: 4, since: Instant::now(), part: "Fotos" };
        assert!(p.remaining().is_none());
        let p = Progress { run_done: 100, since: Instant::now() - Duration::from_secs(100), ..p };
        let r = p.remaining().unwrap().as_secs_f64();
        assert!((r - 1000.0).abs() < 5.0, "{r}");
    }

    #[test]
    fn description_like_ai_scanner() {
        assert_eq!(description_text(r#"["b","a"]"#, "Strand", ""), "a, b. Strand");
    }
}
