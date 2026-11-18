//! People and faces: import from the Lightroom catalog (only boxes and names), search for
//! faces in the photos, features, matching and grouping. The names are keywords with
//! `keyword.person = 1` so that search, filters and sidecars work without detours.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension, Transaction};

fn error(e: rusqlite::Error) -> String {
    e.to_string()
}

fn has_table(c: &Connection, t: &str) -> bool {
    c.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", [t], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0
}

fn number(v: rusqlite::types::Value) -> Option<f64> {
    use rusqlite::types::Value::*;
    match v {
        Integer(i) => Some(i as f64),
        Real(r) => Some(r),
        Text(t) => t.trim().parse().ok(),
        _ => None,
    }
}

/// A face from the Lightroom catalog: (face ID, Lightroom image, box x0 y0 x1 y1, Lightroom keyword).
/// Last two values: confirmed by the user (`userPick`), rejected by the user (`userReject`). Without either, the name is only
/// a suggestion by Lightroom that was never confirmed.
pub type LrFace = (i64, i64, [Option<f64>; 4], Option<i64>, bool, bool);

fn has_column(c: &Connection, table: &str, column: &str) -> bool {
    c.prepare(&format!("SELECT {column} FROM {table} LIMIT 0")).is_ok()
}

/// Clockwise quarter turns of a Lightroom orientation (`AB` normal, `BC` 90°, `CD` 180°, `DA` 270°).
fn lightroom_quarter(o: &str) -> Option<u8> {
    match o.trim() {
        "AB" => Some(0),
        "BC" => Some(1),
        "CD" => Some(2),
        "DA" => Some(3),
        _ => None,
    }
}

/// Box corners (x0 y0 x1 y1) of a face found while the photo stood as `found_as`, in the frame of the photo as it
/// stands now (`now_as`); unchanged if both are the same or unknown.
fn turn_lightroom_box(c: [Option<f64>; 4], found_as: Option<&str>, now_as: Option<&str>) -> [Option<f64>; 4] {
    let (Some(f), Some(n)) = (found_as.and_then(lightroom_quarter), now_as.and_then(lightroom_quarter)) else { return c };
    let [Some(x0), Some(y0), Some(x1), Some(y1)] = c else { return c };
    let q = (n + 4 - f) % 4;
    if q == 0 {
        return c;
    }
    let (x, y, b, h) = crate::face_frame::turn_box(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs(), q);
    [Some(x), Some(y), Some(x + b), Some(y + h)]
}

/// Read face boxes and assignments from the Lightroom catalog (read-only).
pub fn read_lightroom(lr: &Connection) -> Result<Vec<LrFace>, String> {
    if !has_table(lr, "AgLibraryFace") {
        return Ok(Vec::new());
    }
    let keyword_face = has_table(lr, "AgLibraryKeywordFace");
    let part = |column: &str| if keyword_face { format!("(SELECT {column} FROM AgLibraryKeywordFace kf WHERE kf.face = f.id_local ORDER BY kf.userPick DESC, kf.rankOrder LIMIT 1)") } else { "NULL".to_string() };
    let (tag, pick, reject) = (part("kf.tag"), part("kf.userPick"), part("kf.userReject"));
    // orientation of the photo when the face was found and now: a photo rotated in Lightroom afterwards keeps its boxes
    // in the old frame
    let turnable = has_table(lr, "Adobe_images") && has_column(lr, "AgLibraryFace", "imageOrientation");
    let (found_as, now_as) = if turnable { ("f.imageOrientation", "(SELECT i.orientation FROM Adobe_images i WHERE i.id_local = f.image)") } else { ("NULL", "NULL") };
    let mut st = lr
        .prepare(&format!("SELECT f.id_local, f.image, f.tl_x, f.tl_y, f.br_x, f.br_y, {tag}, {pick}, {reject}, {found_as}, {now_as} FROM AgLibraryFace f WHERE coalesce(f.ignored, 0) = 0"))
        .map_err(error)?;
    let r = st
        .query_map([], |r| {
            let v = |i: usize| -> rusqlite::Result<Option<f64>> { Ok(number(r.get::<_, rusqlite::types::Value>(i)?)) };
            let b = |i: usize| -> rusqlite::Result<bool> { Ok(number(r.get::<_, rusqlite::types::Value>(i)?).is_some_and(|x| x > 0.5)) };
            let text = |i: usize| -> Option<String> { r.get::<_, Option<String>>(i).ok().flatten() };
            let corners = turn_lightroom_box([v(2)?, v(3)?, v(4)?, v(5)?], text(9).as_deref(), text(10).as_deref());
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, corners, r.get::<_, Option<i64>>(6)?, b(7)?, b(8)?))
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    Ok(r)
}

/// Write faces into the library. `image_id`: Lightroom image → image, `keyword_id`: Lightroom keyword →
/// keyword. Returns: (faces, of these with a name confirmed by the user). A face that was confirmed or changed in graineo
/// (`local = 1`) keeps its name.
pub fn import_lightroom(tx: &Transaction, rows: &[LrFace], image_id: &HashMap<i64, i64>, keyword_id: &HashMap<i64, i64>) -> Result<(usize, usize), String> {
    let known = crate::lightroom::horizon(tx);
    import_lightroom_with(tx, rows, image_id, keyword_id, known, &std::collections::HashSet::new()).map(|(a, n, _)| (a, n))
}

/// Like [`import_lightroom`]; own changes stay (see [`crate::lightroom`]): a box moved in graineo keeps its place,
/// a face removed in graineo (Lightroom id up to `known`, the horizon of the last import, on a photo not in
/// `new_images`) is not brought back, a face removed in Lightroom goes only if graineo did not change it. Third
/// value: own changes kept.
pub fn import_lightroom_with(
    tx: &Transaction,
    rows: &[LrFace],
    image_id: &HashMap<i64, i64>,
    keyword_id: &HashMap<i64, i64>,
    known: i64,
    new_images: &std::collections::HashSet<i64>,
) -> Result<(usize, usize, usize), String> {
    let old_state = crate::lightroom::state_load(tx, "face").map_err(error)?;
    let mut new_state: Vec<(i64, serde_json::Value)> = Vec::with_capacity(rows.len());
    let mut kept = 0usize;
    // the box of the last import as graineo would hold it now (photo, rect)
    let old_box = |lid: &i64| -> Option<(i64, [f64; 4])> {
        let v = old_state.get(lid)?;
        let image = *image_id.get(&v.get(0)?.as_i64()?)?;
        Some((image, [v.get(1)?.as_f64()?, v.get(2)?.as_f64()?, v.get(3)?.as_f64()?, v.get(4)?.as_f64()?]))
    };
    let mut existing: HashMap<i64, (i64, bool)> = HashMap::new();
    // what each existing face holds now: only rows that really change are written. Rewriting all of them (55 000
    // faces with their feature vectors) took many minutes at background priority and held the write lock all the
    // time; a quit in between threw everything away, so the one-time steps after it never came (0.105.0).
    let mut current: HashMap<i64, (i64, [f64; 4], Option<i64>, String)> = HashMap::new();
    {
        let mut st = tx.prepare("SELECT lr_id, id, local, image, x, y, w, h, person, status FROM face WHERE lr_id IS NOT NULL").map_err(error)?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)? != 0, r.get::<_, i64>(3)?, [r.get::<_, f64>(4)?, r.get::<_, f64>(5)?, r.get::<_, f64>(6)?, r.get::<_, f64>(7)?], r.get::<_, Option<i64>>(8)?, r.get::<_, String>(9)?))
        });
        for r in rows.map_err(error)? {
            let (l, id, local, image, rect, person, status) = r.map_err(error)?;
            existing.insert(l, (id, local));
            current.insert(id, (image, rect, person, status));
        }
    }
    let same_box = |a: &[f64; 4], b: [f64; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-7);
    let now = crate::now();
    let (mut all, mut named) = (0usize, 0usize);
    let mut seen: Vec<i64> = Vec::with_capacity(rows.len());
    {
        let mut new = tx
            .prepare_cached("INSERT INTO face(image, x, y, w, h, source, lr_id, person, status, detected) VALUES(?1,?2,?3,?4,?5,'lightroom',?6,?7,?8,?9)")
            .map_err(error)?;
        let mut rect_list = tx.prepare_cached("UPDATE face SET image=?2, x=?3, y=?4, w=?5, h=?6 WHERE id=?1").map_err(error)?;
        let mut person = tx.prepare_cached("UPDATE face SET person=?2, status=?3 WHERE id=?1 AND local=0").map_err(error)?;
        for (lid, img, p, tag, confirmed, rejected) in rows {
            let (Some(bid), [Some(x0), Some(y0), Some(x1), Some(y1)]) = (image_id.get(img), p) else { continue };
            let (x, y, b, h) = (x0.min(*x1).clamp(0.0, 1.0), y0.min(*y1).clamp(0.0, 1.0), (x1 - x0).abs(), (y1 - y0).abs());
            if b <= 0.0 || h <= 0.0 {
                continue;
            }
            // only what the user confirmed in Lightroom is taken over; Lightroom's own, never confirmed suggestions
            // are deliberately left out (the box stays, the name does not)
            let sid = tag.and_then(|t| keyword_id.get(&t).copied()).filter(|_| *confirmed && !rejected);
            let status = if sid.is_some() { "confirmed" } else { "unknown" };
            seen.push(*lid);
            new_state.push((*lid, serde_json::json!([img, x, y, b, h])));
            match existing.get(lid) {
                Some((id, local)) => {
                    all += 1;
                    named += usize::from(sid.is_some() && *confirmed);
                    let now = current.get(id);
                    if now.is_none_or(|(i, r, _, _)| i != bid || !same_box(r, [x, y, b, h])) {
                        // Lightroom's box only if graineo still has the one of the last import (not for a face set in graineo)
                        let unchanged = match (now, old_box(lid)) {
                            (Some((i, r, _, _)), Some((oi, or))) => *i == oi && same_box(r, or),
                            (None, _) => true,
                            (Some(_), None) => false,
                        };
                        if unchanged && !local {
                            rect_list.execute(params![id, bid, x, y, b, h]).map_err(error)?;
                        } else {
                            kept += 1;
                        }
                    }
                    if !local && now.is_none_or(|(_, _, p, s)| *p != sid || s != status) {
                        person.execute(params![id, sid, status]).map_err(error)?;
                    }
                }
                // existed at the last import, gone now: removed in graineo – stays removed
                None if *lid <= known && !new_images.contains(bid) => kept += 1,
                None => {
                    all += 1;
                    named += usize::from(sid.is_some() && *confirmed);
                    new.execute(params![bid, x, y, b, h, lid, sid, status, now]).map_err(error)?;
                }
            }
        }
    }
    // remove faces removed in Lightroom (without an own change: not set in graineo, box as imported)
    let remove_fn: Vec<i64> = {
        let valid: std::collections::HashSet<i64> = seen.into_iter().collect();
        existing
            .iter()
            .filter(|(l, (id, local))| {
                let moved = match (current.get(id), old_box(l)) {
                    (Some((i, r, _, _)), Some((oi, or))) => *i != oi || !same_box(r, or),
                    _ => false,
                };
                kept += usize::from(!valid.contains(l) && (*local || moved));
                !valid.contains(l) && !local && !moved
            })
            .map(|(_, (id, _))| *id)
            .collect()
    };
    for id in remove_fn {
        tx.execute("DELETE FROM face WHERE id=?1", [id]).map_err(error)?;
    }
    crate::lightroom::state_save(tx, "face", &new_state).map_err(error)?;
    Ok((all, named, kept))
}

/// Version of the Lightroom import rules (changes when the rules change → update once more).
pub const LR_REVISION: &str = "4";

/// Update faces and confirmed names from the Lightroom catalog without repeating the whole import
/// (read-only). Runs once per [`LR_REVISION`]; without a catalog (never imported) nothing happens.
pub fn lightroom_update(db: &crate::db::Db) -> Result<Option<(usize, usize)>, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    if db.info("faces_lr").as_deref() == Some(LR_REVISION) {
        return Ok(None);
    }
    let Some(catalog) = db.info("lightroom_catalog").map(std::path::PathBuf::from).filter(|p| p.exists()) else { return Ok(None) };
    let lr = crate::lightroom::open_catalog(&catalog).map_err(|e| format!("Katalog nicht lesbar: {e}"))?;
    let faces = read_lightroom(&lr)?;
    drop(lr);
    let image_id: HashMap<i64, i64> = {
        let mut st = db.conn.prepare("SELECT lr_id, id FROM image WHERE lr_id IS NOT NULL").map_err(fe)?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(fe)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(fe)?;
        r
    };
    let keyword_id: HashMap<i64, i64> = {
        let mut st = db.conn.prepare("SELECT lr_id, id FROM keyword WHERE lr_id IS NOT NULL").map_err(fe)?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(fe)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(fe)?;
        r
    };
    let tx = db.conn.unchecked_transaction().map_err(fe)?;
    let z = import_lightroom(&tx, &faces, &image_id, &keyword_id)?;
    tx.commit().map_err(fe)?;
    db.set_info("faces_lr", LR_REVISION).map_err(fe)?;
    Ok(Some(z))
}

/// Numbers for report and checking.
pub fn numbers(c: &Connection) -> Result<serde_json::Value, String> {
    let n = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).optional().ok().flatten().unwrap_or(0) };
    Ok(serde_json::json!({
        "faces": n("SELECT count(*) FROM face"),
        "confirmed": n("SELECT count(*) FROM face WHERE status='confirmed'"),
        "suggested": n("SELECT count(*) FROM face WHERE status='suggested'"),
        "unknown": n("SELECT count(*) FROM face WHERE status='unknown'"),
        "with_embedding": n("SELECT count(*) FROM face WHERE embedding IS NOT NULL"),
        "images_scanned": n("SELECT count(*) FROM image WHERE faces_scanned=1"),
        "people": n("SELECT count(DISTINCT person) FROM face WHERE person IS NOT NULL AND status='confirmed'"),
    }))
}


// ───────────────────────────────────────────── Model

/// Location of the model file (SFace): `GRAINEO_FACE_MODEL` or `modules/faces/` in the data folder.
pub fn model_path() -> std::path::PathBuf {
    std::env::var_os("GRAINEO_FACE_MODEL").map(std::path::PathBuf::from).unwrap_or_else(|| crate::data_dir().join("modules").join("faces").join(faces::sface::FILE_NAME))
}

static DOWNLOAD_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The user (or a command line tool) confirmed that the model may be downloaded. Without it, [`load_model`]
/// never touches the network.
pub fn allow_download() {
    DOWNLOAD_ALLOWED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Take the permission back (after a failed download): the next search asks again, with the cause shown.
pub fn revoke_download() {
    DOWNLOAD_ALLOWED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Has the download been allowed (by [`allow_download`]) in this run?
pub fn download_allowed() -> bool {
    DOWNLOAD_ALLOWED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Default of the setting `facesAutomatic`: on where the system finds the faces (macOS, Apple Vision), off where
/// graineo computes the detection itself on the CPU (YuNet: Windows, Linux).
pub fn automatic_default() -> bool {
    faces::detector::AUTOMATIC_DEFAULT
}

/// Is the automatic face search switched on (setting `facesAutomatic`, default [`automatic_default`])?
pub fn automatic_enabled() -> bool {
    crate::settings::value("facesAutomatic").as_bool().unwrap_or(automatic_default())
}

/// Does graineo compute the face detection itself (YuNet on the CPU) instead of the system (Apple Vision)?
pub fn detection_computed_itself() -> bool {
    faces::detector::computes_itself()
}

/// Is the model file there (nothing to download)?
pub fn model_present() -> bool {
    model_path().exists()
}

/// Size of the download in bytes (from the model data).
pub fn model_download_bytes() -> u64 {
    faces::sface::SIZE_BYTES
}

/// Load the model; if the file is missing, it is downloaded once (https, SHA-256 checksum) – but only after
/// [`allow_download`] (the app asks first).
pub fn load_model() -> Result<faces::sface::Sface, String> {
    let p = model_path();
    if !p.exists() {
        if !DOWNLOAD_ALLOWED.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Gesichtsmodell fehlt – es wird erst nach Bestätigung geladen".into());
        }
        download_model(&p)?;
    }
    if std::env::var_os("GRAINEO_FACE_MODEL").is_none() && crate::relocate::sha256(&p)? != faces::sface::SHA256 {
        let _ = std::fs::remove_file(&p);
        return Err("Gesichtsmodell: Prüfsumme stimmt nicht – Datei entfernt, beim nächsten Versuch neu laden".into());
    }
    faces::sface::Sface::load(&p)
}

/// Why the model download failed (shown to the user in plain words by the app; the technical line goes to the log).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadCause {
    /// curl could not be started (not installed, blocked).
    NoCurl,
    /// Proxy, DNS or connection problem (curl 5, 6, 7, 35 via proxy).
    Connection,
    /// Timeout (curl 28).
    Timeout,
    /// TLS / certificate problem (curl 35, 51, 58, 60).
    Tls,
    /// The server answered with an error (curl 22).
    Http,
    /// Could not write the file (curl 23, or the folder is not writable / disk full).
    Write,
    /// The file arrived but the checksum is wrong.
    Checksum,
    Other,
}

/// Last failed download: cause and technical line (for the app's message and log).
#[derive(Clone, Debug)]
pub struct DownloadFailure {
    pub cause: DownloadCause,
    pub technical: String,
}

static LAST_FAILURE: std::sync::Mutex<Option<DownloadFailure>> = std::sync::Mutex::new(None);

/// The failure of the last model download attempt (`None` after a success or before the first attempt).
pub fn last_download_failure() -> Option<DownloadFailure> {
    LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Translate a curl exit code (and its message) into a cause. Exit 56 is "receive error", but with the message
/// "Failure writing output to destination" the target is not writable.
pub fn curl_cause(code: i32, stderr: &str) -> DownloadCause {
    match code {
        56 if stderr.contains("Failure writing output") => DownloadCause::Write,
        5 | 6 | 7 | 52 | 55 | 56 => DownloadCause::Connection,
        28 => DownloadCause::Timeout,
        35 | 51 | 58 | 59 | 60 | 77 | 82 | 83 | 90 | 91 => DownloadCause::Tls,
        22 | 47 => DownloadCause::Http,
        23 => DownloadCause::Write,
        _ => DownloadCause::Other,
    }
}

/// Free space in bytes of the volume holding `path` (macOS/Linux via `df`; `None` where unknown).
fn free_space(path: &std::path::Path) -> Option<u64> {
    let out = std::process::Command::new("df").arg("-k").arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let cols: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    cols.get(3)?.parse::<u64>().ok().map(|k| k * 1024)
}

fn fail(cause: DownloadCause, technical: String) -> DownloadFailure {
    DownloadFailure { cause, technical }
}

/// One curl run for `url` into `part`. Error: cause and technical line (curl exit code and its own message).
fn curl_once(url: &str, part: &std::path::Path) -> Result<(), DownloadFailure> {
    let out = std::process::Command::new("curl")
        .args(["-sSfL", "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "5", "--connect-timeout", "20", "--max-time", "900", "-o"])
        .arg(part)
        .arg(url)
        .output()
        .map_err(|e| fail(DownloadCause::NoCurl, format!("curl could not be started: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let code = out.status.code().unwrap_or(-1);
    let msg = String::from_utf8_lossy(&out.stderr).trim().replace('\n', " ");
    let mut technical = format!("curl exit {code} for {url}: {msg}");
    let cause = curl_cause(code, &msg);
    if cause == DownloadCause::Write {
        let dir = part.parent().unwrap_or(part);
        technical.push_str(&format!(" (target {}", part.display()));
        if let Some(f) = free_space(dir) {
            technical.push_str(&format!(", {} MB free", f / 1_000_000));
        }
        technical.push(')');
    }
    // proxy hints for the log: curl only knows the environment proxies, not the system settings
    for v in ["https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"] {
        if std::env::var_os(v).is_some() {
            technical.push_str(&format!(" [{v} set]"));
        }
    }
    Err(fail(cause, technical))
}

static DOWNLOAD_PART: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
static DOWNLOAD_DONE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Progress of a running model download as (bytes so far, total bytes), read from the growing partial file
/// (no parsing of curl output). `None` when no download is running.
pub fn download_progress() -> Option<(u64, u64)> {
    let part = DOWNLOAD_PART.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    let got = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    Some((got.min(faces::sface::SIZE_BYTES), faces::sface::SIZE_BYTES))
}

/// Did a model download finish successfully within the last `secs` seconds?
pub fn download_done_within(secs: u64) -> bool {
    DOWNLOAD_DONE.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed().as_secs() < secs)
}

fn download_model(target: &std::path::Path) -> Result<(), String> {
    *DOWNLOAD_DONE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let result = download_model_inner(target);
    *DOWNLOAD_PART.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if result.is_ok() {
        *DOWNLOAD_DONE.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    }
    result
}

fn download_model_inner(target: &std::path::Path) -> Result<(), String> {
    let run = || -> Result<(), DownloadFailure> {
        if let Some(d) = target.parent() {
            std::fs::create_dir_all(d).map_err(|e| fail(DownloadCause::Write, format!("folder {} cannot be created: {e}", d.display())))?;
        }
        let part = target.with_extension("onnx.part");
        // a partial file left behind by an older version
        let _ = std::fs::remove_file(target.with_extension("onnx.teil"));
        *DOWNLOAD_PART.lock().unwrap_or_else(|e| e.into_inner()) = Some(part.clone());
        let mut last = None;
        let mut technical = Vec::new();
        for url in [faces::sface::SOURCE_URL, faces::sface::FALLBACK_URL] {
            match curl_once(url, &part) {
                Ok(()) => {
                    let sum = crate::relocate::sha256(&part).map_err(|e| fail(DownloadCause::Write, format!("checksum of {} not readable: {e}", part.display())))?;
                    if sum == faces::sface::SHA256 {
                        return std::fs::rename(&part, target).map_err(|e| fail(DownloadCause::Write, format!("rename to {} failed: {e}", target.display())));
                    }
                    let _ = std::fs::remove_file(&part);
                    technical.push(format!("{url}: checksum {sum} does not match"));
                    last = Some(DownloadCause::Checksum);
                }
                Err(f) => {
                    let _ = std::fs::remove_file(&part);
                    // a write problem will not get better with another host
                    if f.cause == DownloadCause::Write || f.cause == DownloadCause::NoCurl {
                        technical.push(f.technical);
                        return Err(fail(f.cause, technical.join(" | ")));
                    }
                    last = Some(f.cause);
                    technical.push(f.technical);
                }
            }
        }
        Err(fail(last.unwrap_or(DownloadCause::Other), technical.join(" | ")))
    };
    match run() {
        Ok(()) => {
            *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Ok(())
        }
        Err(f) => {
            let text = format!("face model download failed ({:?}): {}", f.cause, f.technical);
            *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
            Err(text)
        }
    }
}

#[cfg(test)]
mod download_tests {
    use super::*;

    #[test]
    fn curl_codes_become_causes() {
        assert_eq!(curl_cause(6, ""), DownloadCause::Connection);
        assert_eq!(curl_cause(7, ""), DownloadCause::Connection);
        assert_eq!(curl_cause(28, ""), DownloadCause::Timeout);
        assert_eq!(curl_cause(35, ""), DownloadCause::Tls);
        assert_eq!(curl_cause(60, ""), DownloadCause::Tls);
        assert_eq!(curl_cause(22, ""), DownloadCause::Http);
        assert_eq!(curl_cause(23, ""), DownloadCause::Write);
        assert_eq!(curl_cause(2, ""), DownloadCause::Other);
        // exit 56: write error of the target versus a real connection loss
        assert_eq!(curl_cause(56, "curl: (56) Failure writing output to destination, passed 1370 returned 0"), DownloadCause::Write);
        assert_eq!(curl_cause(56, "curl: (56) Recv failure: Connection reset by peer"), DownloadCause::Connection);
        assert_eq!(curl_cause(56, ""), DownloadCause::Connection);
    }

    #[test]
    fn unreachable_host_gives_connection_cause() {
        let dir = std::env::temp_dir().join(format!("graineo-dl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let r = curl_once("https://127.0.0.1:1/x.onnx", &dir.join("x.part"));
        let _ = std::fs::remove_dir_all(&dir);
        match r {
            Err(f) => {
                // without curl on the machine the start itself fails
                assert!(matches!(f.cause, DownloadCause::Connection | DownloadCause::NoCurl), "{f:?}");
                assert!(!f.technical.is_empty());
            }
            Ok(()) => panic!("nothing listens on port 1"),
        }
    }
}

// ───────────────────────────────────────────── Face detection (Vision) and features (SFace)

pub const MODEL_NAME: &str = "sface-2021dec";

/// Name of the model used (default SFace; with `GRAINEO_FACE_MODEL` the file name).
pub fn model_name() -> String {
    std::env::var_os("GRAINEO_FACE_MODEL").and_then(|p| std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().into_owned())).filter(|n| n != "sface").unwrap_or_else(|| MODEL_NAME.to_string())
}
/// Longest edge of the image in which faces are searched.
const SEARCH_EDGE: u32 = 2400;

fn search_edge() -> u32 {
    std::env::var("GRAINEO_FACE_EDGE").ok().and_then(|v| v.parse().ok()).unwrap_or(SEARCH_EDGE)
}

pub fn blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn read_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// An image that still needs to be searched for faces.
#[derive(Clone, Debug)]
pub struct Job {
    pub image: i64,
    pub path: std::path::PathBuf,
    pub format: String,
    /// orientation according to the library and upright size (the frame of the boxes, see [`crate::face_frame`])
    pub orientation: Option<i64>,
    pub upright: Option<(u32, u32)>,
}

impl Job {
    /// Job for one image of the library.
    pub fn of(db: &crate::db::Db, image: i64) -> Result<Job, String> {
        let f = crate::face_frame::of(db, image)?;
        Ok(Job { image, path: f.path, format: f.format, orientation: f.orientation, upright: f.upright })
    }

    fn frame(&self) -> crate::face_frame::Frame {
        crate::face_frame::Frame { path: self.path.clone(), format: self.format.clone(), orientation: self.orientation, upright: self.upright, key: 0 }
    }
}

/// Images without face search (originals only; not camera JPEGs in the stack or virtual copies), images with
/// Lightroom faces first, then the newest.
pub fn open_jobs(db: &crate::db::Db, max: usize) -> Result<Vec<Job>, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let open_set: std::collections::HashSet<i64> = {
        let mut st = db.conn.prepare("SELECT id FROM image WHERE faces_scanned = 0").map_err(fe)?;
        let r = st.query_map([], |r| r.get(0)).map_err(fe)?.collect::<rusqlite::Result<_>>().map_err(fe)?;
        r
    };
    // photos with confirmed faces first: they are the basis for everything else
    let with_face: std::collections::HashSet<i64> = {
        let mut st = db.conn.prepare("SELECT DISTINCT image FROM face WHERE status='confirmed'").map_err(fe)?;
        let r = st.query_map([], |r| r.get(0)).map_err(fe)?.collect::<rusqlite::Result<_>>().map_err(fe)?;
        r
    };
    let mut rows = db.rows().map_err(fe)?;
    crate::db::mark_pairs(&mut rows);
    let paths = db.folder_paths().map_err(fe)?;
    let mut v: Vec<(bool, Option<String>, Job)> = Vec::new();
    for z in &rows {
        if z.copy.is_some() || z.jpeg_for.is_some() || !open_set.contains(&z.image) || z.format == "VIDEO" {
            continue;
        }
        if z.status != 0 {
            continue;
        }
        let (folder, name) = (z.folder, z.name.clone());
        let Some(dir) = paths.get(&folder) else { continue };
        v.push((with_face.contains(&z.image), z.capture.clone(), Job { image: z.image, path: std::path::Path::new(dir).join(name), format: z.format.clone(), orientation: z.orientation, upright: crate::face_frame::upright_size(z.width, z.height, z.orientation) }));
    }
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    v.truncate(max);
    Ok(v.into_iter().map(|x| x.2).collect())
}

/// A detected face with its feature, ready to be saved (box and points normalized).
struct RawEntry {
    x: f32,
    y: f32,
    b: f32,
    h: f32,
    points_list: Option<[f32; 10]>,
    embedding: Vec<f32>,
    quality: f32,
}

struct Finding {
    image: i64,
    outcome: Result<Vec<RawEntry>, String>,
    /// the failure is the photo itself (its file is there but cannot be decoded): not searched again. Otherwise (file
    /// not reachable, Vision or the model failing) the photo is tried again at the next run.
    permanent: bool,
}

/// `faces_scanned` of a photo whose search failed for a passing reason (see [`Finding::permanent`]): left out for
/// the rest of the run, open again at the next [`run`].
const SEARCH_RETRY: i64 = 3;

/// Sharpness measure 0…1 of the aligned face (variance of the Laplacian on grey).
fn sharpness(rgb112: &[u8]) -> f32 {
    let s = faces::align::SIDE;
    let gray: Vec<f32> = rgb112.chunks_exact(3).map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).collect();
    let (mut sums, mut sum2, mut n) = (0f64, 0f64, 0f64);
    for y in 1..s - 1 {
        for x in 1..s - 1 {
            let l = 4.0 * gray[y * s + x] - gray[y * s + x - 1] - gray[y * s + x + 1] - gray[(y - 1) * s + x] - gray[(y + 1) * s + x];
            sums += l as f64;
            sum2 += (l * l) as f64;
            n += 1.0;
        }
    }
    let var = (sum2 / n - (sums / n).powi(2)).max(0.0) as f32;
    (var / 120.0).clamp(0.0, 1.0)
}

fn analyze(a: &Job, model: &faces::sface::Sface) -> Finding {
    let mut permanent = false;
    let outcome = (|| -> Result<Vec<RawEntry>, String> {
        // the image the boxes refer to: upright as the library shows it, for photos cropped in Apple Photos the current version
        let image = crate::face_frame::load(&a.frame(), search_edge()).inspect_err(|_| permanent = a.path.is_file())?;
        let (w, h) = (image.width() as f32, image.height() as f32);
        let found = if std::env::var_os("GRAINEO_FACE_NO_TILES").is_some() { faces::detector::detect(&image)? } else { faces::detector::detect_with_tiles(&image)? };
        let mut out = Vec::new();
        for g in found {
            let size = (g.b * g.h).sqrt();
            if g.confidence < 0.5 || size < 24.0 {
                continue;
            }
            let points_list = g.points.unwrap_or_else(|| faces::align::points_from_box(g.x, g.y, g.b, g.h));
            let crop = faces::align::crop_aligned(&image, &points_list);
            let embedding = model.embedding(&crop)?;
            // the network detects faces turned sideways or upside down poorly: reduce quality
            let gaze = (1.0 - 0.5 * (g.yaw.unwrap_or(0.0).abs() / 0.8).min(1.0)) * (1.0 - 0.6 * (g.roll.unwrap_or(0.0).abs() / 0.8).min(1.0));
            let quality = (size / 90.0).min(1.0) * gaze * (0.4 + 0.6 * sharpness(&crop));
            out.push(RawEntry {
                x: (g.x / w).clamp(0.0, 1.0),
                y: (g.y / h).clamp(0.0, 1.0),
                b: (g.b / w).min(1.0),
                h: (g.h / h).min(1.0),
                points_list: g.points.map(|p| {
                    let mut o = [0f32; 10];
                    for (i, q) in p.iter().enumerate() {
                        o[2 * i] = q.0 / w;
                        o[2 * i + 1] = q.1 / h;
                    }
                    o
                }),
                embedding,
                quality,
            });
        }
        Ok(out)
    })();
    Finding { image: a.image, outcome, permanent }
}

/// Overlap of a found face with an existing box (Lightroom boxes are larger: hair, chin):
/// fraction of the found box that lies within the existing one, divided by area – only if the center is inside.
pub(crate) fn overlap(new: (f32, f32, f32, f32), old: (f32, f32, f32, f32)) -> f32 {
    let (nx, ny, nb, nh) = new;
    let (ax, ay, ab, ah) = old;
    let (mx, my) = (nx + nb / 2.0, ny + nh / 2.0);
    if mx < ax || mx > ax + ab || my < ay || my > ay + ah {
        return 0.0;
    }
    let iw = (nx + nb).min(ax + ab) - nx.max(ax);
    let ih = (ny + nh).min(ay + ah) - ny.max(ay);
    if iw <= 0.0 || ih <= 0.0 {
        return 0.0;
    }
    let cut = iw * ih;
    let union_set = nb * nh + ab * ah - cut;
    // mix of "lies inside" and IoU: boxes of equal size and Lightroom boxes (≈ 2× area) count
    (cut / (nb * nh)) * 0.6 + (cut / union_set) * 0.4
}

/// Result of a search run.
#[derive(Debug, Default, Clone)]
pub struct SearchReport {
    pub images: usize,
    pub error: usize,
    /// of these, failed for a passing reason (tried again at the next run)
    pub retry: usize,
    pub faces: usize,
    /// of these, joined with a Lightroom box
    pub connected: usize,
    pub new: usize,
    pub ms: u128,
    /// what the one-time steps after an update did ([`once_after_update`]), for the log
    pub notes: Vec<String>,
}

/// Search for faces (at most `max` images): find, align, compute the feature, join with existing
/// Lightroom boxes and save. `progress(finished, total)` is called after each batch.
pub fn search(db: &crate::db::Db, model: &faces::sface::Sface, max: usize, threads: usize, cancel: &std::sync::atomic::AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<SearchReport, String> {
    let jobs = open_jobs(db, max)?;
    search_jobs(db, model, jobs, threads, cancel, progress)
}

/// Like [`search`], but for a given list of images.
pub fn search_jobs(db: &crate::db::Db, model: &faces::sface::Sface, jobs: Vec<Job>, threads: usize, cancel: &std::sync::atomic::AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<SearchReport, String> {
    use rayon::prelude::*;
    let fe = |e: rusqlite::Error| e.to_string();
    let t0 = std::time::Instant::now();
    let total = jobs.len();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).start_handler(|_| crate::platform::background_priority()).build().map_err(|e| e.to_string())?;
    let mut report = SearchReport::default();
    for batch in jobs.chunks(threads.max(1) * 6) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let findings: Vec<Finding> = pool.install(|| batch.par_iter().map(|a| analyze(a, model)).collect());
        let tx = db.conn.unchecked_transaction().map_err(fe)?;
        for finding in findings {
            let now = crate::now();
            match finding.outcome {
                Err(_) => {
                    // before 0.108 every failure counted for good (state 2): a Vision outage or a sleeping NAS
                    // closed the face search for whole libraries without a way back
                    let state = if finding.permanent { 2 } else { SEARCH_RETRY };
                    tx.execute("UPDATE image SET faces_scanned=?2 WHERE id=?1", [finding.image, state]).map_err(fe)?;
                    report.error += 1;
                    if !finding.permanent {
                        report.retry += 1;
                    }
                }
                Ok(raw_refs) => {
                    // existing faces of this image (Lightroom and Apple boxes, own finds with a name when searched again);
                    // earlier own finds without a person are replaced
                    tx.execute("DELETE FROM face WHERE image=?1 AND source='graineo' AND person IS NULL AND local=0", [finding.image]).map_err(fe)?;
                    let old: Vec<(i64, (f32, f32, f32, f32))> = {
                        let mut st = tx.prepare_cached("SELECT id, x, y, w, h FROM face WHERE image=?1").map_err(fe)?;
                        let r = st
                            .query_map([finding.image], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, f64>(1)? as f32, r.get::<_, f64>(2)? as f32, r.get::<_, f64>(3)? as f32, r.get::<_, f64>(4)? as f32))))
                            .map_err(fe)?
                            .collect::<rusqlite::Result<Vec<_>>>()
                            .map_err(fe)?;
                        r
                    };
                    let mut assigned: std::collections::HashSet<i64> = std::collections::HashSet::new();
                    // best pairs first
                    let mut pairs: Vec<(f32, usize, i64)> = Vec::new();
                    for (i, r) in raw_refs.iter().enumerate() {
                        for (id, ar) in &old {
                            let d = overlap((r.x, r.y, r.b, r.h), *ar);
                            if d > 0.45 {
                                pairs.push((d, i, *id));
                            }
                        }
                    }
                    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
                    let mut to: HashMap<usize, i64> = HashMap::new();
                    for (_, i, id) in pairs {
                        if !to.contains_key(&i) && !assigned.contains(&id) {
                            to.insert(i, id);
                            assigned.insert(id);
                        }
                    }
                    for (i, r) in raw_refs.iter().enumerate() {
                        let points_list = r.points_list.map(|p| blob(&p));
                        report.faces += 1;
                        match to.get(&i) {
                            Some(id) => {
                                tx.execute(
                                    "UPDATE face SET landmarks=?2, embedding=?3, model=?4, quality=?5, detected=?6 WHERE id=?1",
                                    params![id, points_list, blob(&r.embedding), model_name(), r.quality, now],
                                )
                                .map_err(fe)?;
                                report.connected += 1;
                            }
                            None => {
                                tx.execute(
                                    "INSERT INTO face(image, x, y, w, h, landmarks, embedding, model, quality, source, status, detected) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'graineo','unknown',?10)",
                                    params![finding.image, r.x, r.y, r.b, r.h, points_list, blob(&r.embedding), model_name(), r.quality, now],
                                )
                                .map_err(fe)?;
                                report.new += 1;
                            }
                        }
                    }
                    tx.execute("UPDATE image SET faces_scanned=1 WHERE id=?1", [finding.image]).map_err(fe)?;
                }
            }
            report.images += 1;
        }
        tx.commit().map_err(fe)?;
        progress(report.images, total);
    }
    report.ms = t0.elapsed().as_millis();
    Ok(report)
}


// ───────────────────────────────────────────── Matching

/// Face with feature, as needed for matching and measuring.
pub struct Probe {
    pub id: i64,
    pub image: i64,
    pub person: Option<i64>,
    pub status: String,
    pub local: bool,
    /// assigned automatically from an Apple group (`lokal = 2`): protected like `local`, but still checked for doubts
    pub auto: bool,
    pub quality: f32,
    pub embedding: Vec<f32>,
    /// folder of the photo (for the strict "leave folder out" measurement)
    pub folder: i64,
}

pub fn load_samples(c: &Connection) -> Result<Vec<Probe>, String> {
    // faces with a suspicious name do not count as references
    let mut st = c.prepare("SELECT g.id, g.image, g.person, CASE WHEN g.suspect=1 AND g.status='confirmed' THEN 'suspect' ELSE g.status END, g.local, coalesce(g.quality,0), g.embedding, coalesce(d.folder,0)
         FROM face g LEFT JOIN image b ON b.id=g.image LEFT JOIN file d ON d.id=b.file WHERE g.embedding IS NOT NULL").map_err(error)?;
    let r = st
        .query_map([], |r| {
            Ok(Probe { id: r.get(0)?, image: r.get(1)?, person: r.get(2)?, status: r.get(3)?, local: r.get::<_, i64>(4)? != 0, auto: r.get::<_, i64>(4)? == crate::apple_groups::LOCAL_APPLE_GROUP, quality: r.get::<_, f64>(5)? as f32, embedding: read_blob(&r.get::<_, Vec<u8>>(6)?), folder: r.get(7)? })
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    // only features of one length (the most common one: the model's) without NaN: one of another length (damaged row,
    // another model) stopped the whole matching run with a panic in the similarity computation
    let mut lengths: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for p in &r {
        *lengths.entry(p.embedding.len()).or_default() += 1;
    }
    let len = lengths.into_iter().max_by_key(|&(l, n)| (n, l)).map_or(0, |x| x.0);
    let r = r.into_iter().filter(|p| p.embedding.len() == len && len > 0 && p.embedding.iter().all(|x| x.is_finite())).collect();
    Ok(r)
}

/// Thresholds and method of the matching (from own measurement).
#[derive(Clone, Debug)]
pub struct Thresholds {
    /// mean of the best `k` similarities per person
    pub k: usize,
    /// references must have at least this quality
    pub min_quality: f32,
    /// from here "certain"
    pub safe: f32,
    /// from here suggestion
    pub suggestion: f32,
    /// required margin to the second-best person for "certain"
    pub distance: f32,
    /// faces without a person with at least this similarity belong to a group
    pub group: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        // From the measurement on 6,525 confirmed faces (folder left out) and the faces of unknown people:
        // from 0.60, 2.8 % of strangers get a wrong suggestion (with margin 0.10), from 0.70 only 0.8 %.
        // Grouping (average linkage) measured on confirmed faces: purity 99.5 % with 93 % of the faces in groups (from 0.60).
        Thresholds { k: 3, min_quality: 0.25, safe: 0.70, suggestion: 0.60, distance: 0.10, group: 0.60 }
    }
}

/// Best person per query: (person, score, second-best score). `ausser_bild`: references of the same image do not count
/// ("leave photo out" measurement). `never`: people excluded per query.
pub struct Match {
    pub person: i64,
    pub value: f32,
    pub second_item: f32,
}

/// `except`: 0 leave nothing out, 1 faces of the same photo, 2 faces of the same folder (strict measurement),
/// 3 additionally all faces of the same person (measurement for strangers: every suggestion is wrong).
pub fn best_person(queries: &[&Probe], refs: &[&Probe], k: usize, except: u8, never: &HashMap<i64, Vec<i64>>) -> Vec<Option<Match>> {
    use rayon::prelude::*;
    if refs.is_empty() {
        return queries.iter().map(|_| None).collect();
    }
    let d = refs[0].embedding.len();
    // sort references by person
    let mut order: Vec<usize> = (0..refs.len()).collect();
    order.sort_by_key(|&i| refs[i].person);
    let ref_mat: Vec<f32> = order.iter().flat_map(|&i| refs[i].embedding.iter().copied()).collect();
    let mut regions: Vec<(i64, usize, usize)> = Vec::new();
    let mut start = 0;
    for i in 1..=order.len() {
        if i == order.len() || refs[order[i]].person != refs[order[start]].person {
            regions.push((refs[order[start]].person.unwrap_or(0), start, i));
            start = i;
        }
    }
    let mut out: Vec<Option<Match>> = Vec::with_capacity(queries.len());
    for batch in queries.chunks(256) {
        let query_mat: Vec<f32> = batch.iter().flat_map(|p| p.embedding.iter().copied()).collect();
        let sims = faces::matching::similarities(&query_mat, batch.len(), &ref_mat, refs.len(), d);
        let part: Vec<Option<Match>> = batch
            .par_iter()
            .enumerate()
            .map(|(qi, q)| {
                let row = &sims[qi * refs.len()..(qi + 1) * refs.len()];
                let excluded = never.get(&q.id);
                let (mut b1, mut b2): (Option<(i64, f32)>, f32) = (None, -1.0);
                for &(person, a, e) in &regions {
                    if excluded.is_some_and(|v| v.contains(&person)) {
                        continue;
                    }
                    // best k scores of this person (without the query itself and possibly without faces of the same image)
                    let mut top = [-2f32; 8];
                    let kk = k.clamp(1, 8);
                    let mut n = 0;
                    for j in a..e {
                        let r = refs[order[j]];
                        if r.id == q.id || (except >= 1 && r.image == q.image) || (except >= 2 && r.folder == q.folder) || (except >= 3 && r.person == q.person) {
                            continue;
                        }
                        let s = row[j];
                        if n < kk {
                            top[n] = s;
                            n += 1;
                            top[..n].sort_by(|x, y| y.total_cmp(x));
                        } else if s > top[kk - 1] {
                            top[kk - 1] = s;
                            top[..kk].sort_by(|x, y| y.total_cmp(x));
                        }
                    }
                    if n == 0 {
                        continue;
                    }
                    let value = top[..n].iter().sum::<f32>() / n as f32;
                    match b1 {
                        Some((_, w)) if value <= w => {
                            if value > b2 {
                                b2 = value;
                            }
                        }
                        _ => {
                            if let Some((_, w)) = b1 {
                                b2 = w;
                            }
                            b1 = Some((person, value));
                        }
                    }
                }
                b1.map(|(person, value)| Match { person, value, second_item: b2.max(-1.0) })
            })
            .collect();
        out.extend(part);
    }
    out
}

/// "Leave photo out" measurement: each confirmed face is checked against all other confirmed faces from other images.
/// Output: text with hit rate by quality and the curve threshold → precision / coverage.
pub fn score(c: &Connection, k: usize, max_samples: usize, except: u8) -> Result<String, String> {
    let all = load_samples(c)?;
    let refs: Vec<&Probe> = all.iter().filter(|p| p.status == "confirmed" && p.person.is_some() && p.quality >= 0.25).collect();
    let mut queries: Vec<&Probe> = refs.clone();
    // select evenly
    if queries.len() > max_samples {
        let step = queries.len() as f64 / max_samples as f64;
        queries = (0..max_samples).map(|i| queries[(i as f64 * step) as usize]).collect();
    }
    let t = std::time::Instant::now();
    let matches = best_person(&queries, &refs, k, except, &HashMap::new());
    let mut text = format!("Messung: {} Abfragen gegen {} Referenzen (k = {k}, weggelassen: {}), {:?}\n", queries.len(), refs.len(), match except { 0 => "nichts", 1 => "dasselbe Foto", 2 => "derselbe Ordner", _ => "dieselbe Person (Fremde)" }, t.elapsed());
    let correct = |q: &Probe, t: &Option<Match>| t.as_ref().is_some_and(|t| Some(t.person) == q.person);
    let n = queries.len().max(1);
    let ok = queries.iter().zip(&matches).filter(|(q, t)| correct(q, t)).count();
    text += &format!("Trefferquote gesamt: {:.1} % ({} von {})\n", 100.0 * ok as f64 / n as f64, ok, queries.len());
    for (name, a, e) in [("Qualität ≥ 0,6", 0.6f32, 9.0f32), ("0,4 – 0,6", 0.4, 0.6), ("0,25 – 0,4", 0.25, 0.4)] {
        let (mut n_total, mut good) = (0, 0);
        for (q, t) in queries.iter().zip(&matches) {
            if q.quality >= a && q.quality < e {
                n_total += 1;
                good += usize::from(correct(q, t));
            }
        }
        if n_total > 0 {
            text += &format!("  {name}: {:.1} % ({good} von {n_total})\n", 100.0 * good as f64 / n_total as f64);
        }
    }
    // per person: hit rate and most frequent confusion
    let names: HashMap<i64, String> = {
        let mut st = c.prepare("SELECT id, name FROM keyword WHERE person=1").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(error)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(error)?;
        r
    };
    let mut per: HashMap<i64, (usize, usize, HashMap<i64, usize>)> = HashMap::new();
    for (q, t) in queries.iter().zip(&matches) {
        let Some(p) = q.person else { continue };
        let e = per.entry(p).or_default();
        e.0 += 1;
        match t {
            Some(tr) if tr.person == p => e.1 += 1,
            Some(tr) => *e.2.entry(tr.person).or_default() += 1,
            None => {}
        }
    }
    let mut list: Vec<_> = per.into_iter().filter(|(_, v)| v.0 >= 25).collect();
    list.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    text += "Je Person (ab 25 Gesichtern): Quote, häufigste Verwechslung\n";
    for (p, (n, good, wrong)) in list.iter().take(24) {
        let confused_with = wrong.iter().max_by_key(|(_, c)| **c).map(|(q, c)| format!("{} ({c})", names.get(q).map_or("?", |s| s.as_str()))).unwrap_or_default();
        text += &format!("  {:<18} {:>4} Gesichter  {:>5.1} %   verwechselt mit {confused_with}\n", names.get(p).map_or("?", |s| s.as_str()), n, 100.0 * *good as f64 / *n as f64);
    }
    if except >= 3 {
        text += "Gesichter unbekannter Personen (alle Gesichter der Person weggelassen): Anteil, der trotzdem einen (falschen) Namensvorschlag bekäme:\n";
        for st in [0.45f32, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80] {
            let (mut a, mut m, mut m10) = (0, 0, 0);
            for t in matches.iter().flatten() {
                if t.value >= st {
                    a += 1;
                    m += usize::from(t.value - t.second_item >= 0.05);
                    m10 += usize::from(t.value - t.second_item >= 0.10);
                }
            }
            text += &format!("  ab {st:.2}: {:.1} % (Abstand ≥ 0,05: {:.1} %, Abstand ≥ 0,10: {:.1} %)\n", 100.0 * a as f64 / n as f64, 100.0 * m as f64 / n as f64, 100.0 * m10 as f64 / n as f64);
        }
        return Ok(text);
    }
    text += "Schwelle (bester Wert) → Genauigkeit der Vorschläge / Anteil der Gesichter mit Vorschlag:\n";
    for s in [0.30f32, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65] {
        let (mut prior, mut good) = (0, 0);
        for (q, t) in queries.iter().zip(&matches) {
            if let Some(tr) = t {
                if tr.value >= s {
                    prior += 1;
                    good += usize::from(Some(tr.person) == q.person);
                }
            }
        }
        text += &format!("  ≥ {s:.2}: Genauigkeit {:.1} %, Abdeckung {:.1} % ({prior})\n", if prior > 0 { 100.0 * good as f64 / prior as f64 } else { 0.0 }, 100.0 * prior as f64 / n as f64);
    }
    text += "Mit Abstand zur zweitbesten Person ≥ 0,10 und Schwelle:\n";
    for s in [0.45f32, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80] {
        let (mut prior, mut good) = (0, 0);
        for (q, t) in queries.iter().zip(&matches) {
            if let Some(tr) = t {
                if tr.value >= s && tr.value - tr.second_item >= 0.10 {
                    prior += 1;
                    good += usize::from(Some(tr.person) == q.person);
                }
            }
        }
        text += &format!("  ≥ {s:.2}: Genauigkeit {:.1} %, Abdeckung {:.1} % ({prior})\n", if prior > 0 { 100.0 * good as f64 / prior as f64 } else { 0.0 }, 100.0 * prior as f64 / n as f64);
    }
    Ok(text)
}


/// Check helper: mismatches of the measurement as a contact sheet (query left, wrongly chosen neighbor right).
pub fn error_images(db: &crate::db::Db, count: usize, output: &std::path::Path) -> Result<String, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let all = load_samples(&db.conn)?;
    let refs: Vec<&Probe> = all.iter().filter(|p| p.status == "confirmed" && p.person.is_some() && p.quality >= 0.25).collect();
    let d = refs.first().ok_or("keine Referenzen")?.embedding.len();
    let ref_mat: Vec<f32> = refs.iter().flat_map(|p| p.embedding.iter().copied()).collect();
    let queries: Vec<&&Probe> = refs.iter().filter(|p| p.quality >= 0.7).take(1500).collect();
    let query_mat: Vec<f32> = queries.iter().flat_map(|p| p.embedding.iter().copied()).collect();
    let sims = faces::matching::similarities(&query_mat, queries.len(), &ref_mat, refs.len(), d);
    let mut error: Vec<(usize, usize, f32)> = Vec::new();
    for (qi, q) in queries.iter().enumerate() {
        let (mut b, mut bs) = (None, -2.0f32);
        for (j, r) in refs.iter().enumerate() {
            if r.image == q.image {
                continue;
            }
            let s = sims[qi * refs.len() + j];
            if s > bs {
                bs = s;
                b = Some(j);
            }
        }
        if let Some(j) = b {
            if refs[j].person != q.person && bs >= 0.5 {
                error.push((qi, j, bs));
            }
        }
    }
    let names: HashMap<i64, String> = {
        let mut st = db.conn.prepare("SELECT id, name FROM keyword WHERE person=1").map_err(fe)?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(fe)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(fe)?;
        r
    };
    let rows = db.rows().map_err(fe)?;
    let paths = db.folder_paths().map_err(fe)?;
    let place: HashMap<i64, (std::path::PathBuf, String)> = rows
        .iter()
        .filter(|z| z.copy.is_none())
        .filter_map(|z| {
            paths.get(&z.folder).map(|d| (z.image, (std::path::Path::new(d).join(&z.name), z.format.clone())))
        })
        .collect();
    let mut text = format!("{} Fehlzuordnungen bei Qualität ≥ 0,7 und Ähnlichkeit ≥ 0,5 von {} Abfragen\n", error.len(), queries.len());
    let step = (error.len() / count.max(1)).max(1);
    let selection: Vec<_> = error.iter().step_by(step).take(count).collect();
    let side = 128u32;
    let mut arc = image::RgbImage::new(side * 2 + 8, (side + 4) * selection.len() as u32);
    for (zi, (qi, j, s)) in selection.iter().enumerate() {
        let (q, r) = (queries[*qi], refs[*j]);
        text += &format!("Zeile {}: {} (Wahrheit) ← verwechselt mit {} (Ähnlichkeit {s:.2}), Qualität {:.2}\n", zi + 1, names.get(&q.person.unwrap_or(0)).map_or("?", |x| x.as_str()), names.get(&r.person.unwrap_or(0)).map_or("?", |x| x.as_str()), q.quality);
        for (k, p) in [q, r].iter().enumerate() {
            let rect_list: Option<(f64, f64, f64, f64)> = db.conn.query_row("SELECT x, y, w, h FROM face WHERE id=?1", [p.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).ok();
            let (Some((x, y, b, h)), Some(_)) = (rect_list, place.get(&p.image)) else { continue };
            let Ok(image) = crate::face_frame::of(db, p.image).and_then(|f| crate::face_frame::load(&f, 1600)) else { continue };
            let (w, hh) = (image.width() as f64, image.height() as f64);
            let (cx, cy, side_len) = ((x + b / 2.0) * w, (y + h / 2.0) * hh, (b * w).max(h * hh) * 1.3);
            let (x0, y0) = ((cx - side_len / 2.0).max(0.0), (cy - side_len / 2.0).max(0.0));
            let crop = image::imageops::crop_imm(&image, x0 as u32, y0 as u32, (side_len as u32).min(image.width() - x0 as u32).max(1), (side_len as u32).min(image.height() - y0 as u32).max(1)).to_image();
            let small = image::imageops::resize(&crop, side, side, image::imageops::FilterType::Triangle);
            image::imageops::replace(&mut arc, &small, (k as u32 * (side + 8)) as i64, (zi as u32 * (side + 4)) as i64);
        }
    }
    arc.save(output).map_err(|e| e.to_string())?;
    Ok(text)
}


// ───────────────────────────────────────────── Detecting wrong names

/// Per query: score of the own person and of the best other person (mean of the best `k` similarities; faces
/// of the same image do not count).
fn own_and_foreign(queries: &[&Probe], refs: &[&Probe], k: usize, never: &HashMap<i64, Vec<i64>>) -> Vec<(Option<f32>, Option<(i64, f32)>)> {
    use rayon::prelude::*;
    let d = refs[0].embedding.len();
    let mut order: Vec<usize> = (0..refs.len()).collect();
    order.sort_by_key(|&i| refs[i].person);
    let ref_mat: Vec<f32> = order.iter().flat_map(|&i| refs[i].embedding.iter().copied()).collect();
    let mut regions: Vec<(i64, usize, usize)> = Vec::new();
    let mut start = 0;
    for i in 1..=order.len() {
        if i == order.len() || refs[order[i]].person != refs[order[start]].person {
            regions.push((refs[order[start]].person.unwrap_or(0), start, i));
            start = i;
        }
    }
    let mut out = Vec::with_capacity(queries.len());
    for batch in queries.chunks(256) {
        let query_mat: Vec<f32> = batch.iter().flat_map(|p| p.embedding.iter().copied()).collect();
        let sims = faces::matching::similarities(&query_mat, batch.len(), &ref_mat, refs.len(), d);
        let part: Vec<(Option<f32>, Option<(i64, f32)>)> = batch
            .par_iter()
            .enumerate()
            .map(|(qi, q)| {
                let row = &sims[qi * refs.len()..(qi + 1) * refs.len()];
                let (mut own, mut foreign): (Option<f32>, Option<(i64, f32)>) = (None, None);
                for &(person, a, e) in &regions {
                    let mut top: Vec<f32> = Vec::new();
                    for j in a..e {
                        let r = refs[order[j]];
                        if r.id == q.id || r.image == q.image {
                            continue;
                        }
                        top.push(row[j]);
                    }
                    if top.is_empty() {
                        continue;
                    }
                    top.sort_by(|x, y| y.total_cmp(x));
                    top.truncate(k);
                    let w = top.iter().sum::<f32>() / top.len() as f32;
                    if Some(person) == q.person {
                        own = Some(w);
                    } else if never.get(&q.id).is_some_and(|v| v.contains(&person)) {
                        // excluded by a human: do not suggest again
                    } else if foreign.is_none_or(|(_, b)| w > b) {
                        foreign = Some((person, w));
                    }
                }
                (own, foreign)
            })
            .collect();
        out.extend(part);
    }
    out
}

/// Detect wrongly named faces: a confirmed face is suspicious if it resembles another person clearly more
/// than all other faces of its own (difference ≥ `distance`, similarity to the other ≥ `minimum`).
/// Suspicious ones do not count as references (`face.suspects = 1`); the name stays unchanged. The check runs twice
/// (after the first exclusion the references are cleaner). Returns: report with people who apparently exist twice.
pub fn clean_up(c: &Connection, distance: f32, minimum: f32, write: bool) -> Result<String, String> {
    let k = 3;
    let mut all = load_samples(c)?;
    let names: HashMap<i64, String> = {
        let mut st = c.prepare("SELECT id, name FROM keyword WHERE person=1").map_err(error)?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(error)?.collect::<rusqlite::Result<HashMap<_, _>>>().map_err(error)?;
        r
    };
    let never: HashMap<i64, Vec<i64>> = {
        let mut m: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut st = c.prepare("SELECT face, person FROM face_never").map_err(error)?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(error)? {
            let (g, p) = r.map_err(error)?;
            m.entry(g).or_default().push(p);
        }
        m
    };
    let mut detail: HashMap<i64, (i64, f32, f32)> = HashMap::new();
    let mut suspects: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut text = String::new();
    let mut opposite: HashMap<(i64, i64), usize> = HashMap::new();
    for round in 1..=2 {
        for p in all.iter_mut() {
            if p.status == "suspect" || suspects.contains(&p.id) {
                p.status = "suspect".into();
            }
        }
        let refs: Vec<&Probe> = all.iter().filter(|p| p.status == "confirmed" && p.person.is_some() && p.quality >= 0.25).collect();
        if refs.is_empty() {
            return Err("keine bestätigten Gesichter mit Merkmal".into());
        }
        let queries: Vec<&Probe> = all.iter().filter(|p| (p.status == "confirmed" || p.status == "suspect") && p.person.is_some() && p.quality >= 0.25).collect();
        let w = own_and_foreign(&queries, &refs, k, &never);
        let mut new: Vec<i64> = Vec::new();
        opposite.clear();
        for (q, (own, foreign)) in queries.iter().zip(&w) {
            let Some((fp, fw)) = foreign else { continue };
            if q.local && !q.auto {
                continue; // confirmed or corrected by a human
            }
            let e = own.unwrap_or(-1.0);
            *opposite.entry((q.person.unwrap(), *fp)).or_default() += usize::from(fw - e >= distance && *fw >= minimum);
            if fw - e >= distance && *fw >= minimum {
                new.push(q.id);
                detail.insert(q.id, (*fp, e, *fw));
            } else if suspects.contains(&q.id) && fw - e < distance * 0.5 {
                // no longer suspicious
                suspects.remove(&q.id);
                detail.remove(&q.id);
            }
        }
        for id in new {
            suspects.insert(id);
        }
        text += &format!("Runde {round}: {} Gesichter geprüft, {} verdächtig (Abstand ≥ {distance}, Ähnlichkeit ≥ {minimum})\n", queries.len(), suspects.len());
    }
    // people with many suspicious faces and confusion pairs
    let mut per_person: HashMap<i64, (usize, usize)> = HashMap::new();
    for p in &all {
        if let Some(pe) = p.person.filter(|_| p.status == "confirmed" || p.status == "suspect") {
            let e = per_person.entry(pe).or_default();
            e.0 += 1;
            e.1 += usize::from(suspects.contains(&p.id));
        }
    }
    let mut v: Vec<_> = per_person.iter().filter(|(_, (n, _))| *n >= 20).collect();
    v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    text += "Personen mit den meisten verdächtigen Gesichtern:\n";
    for (p, (n, s)) in v.iter().take(12) {
        text += &format!("  {:<18} {s:>4} von {n:>4} ({:.0} %)\n", names.get(p).map_or("?", |x| x.as_str()), 100.0 * *s as f64 / *n as f64);
    }
    let mut pairs: Vec<_> = opposite.iter().filter(|(_, c)| **c >= 8).collect();
    pairs.sort_by(|a, b| b.1.cmp(a.1));
    text += "Häufigste Verwechslungen (Wahrheit → ähnelt eher …), mögliche doppelte Personen:\n";
    for ((p, q), c) in pairs.iter().take(15) {
        text += &format!("  {} → {}: {c}\n", names.get(p).map_or("?", |x| x.as_str()), names.get(q).map_or("?", |x| x.as_str()));
    }
    if write {
        let tx = c.unchecked_transaction().map_err(error)?;
        tx.execute("UPDATE face SET suspect=0, guessed=NULL, score_own=NULL, score_guessed=NULL WHERE local<>1", []).map_err(error)?;
        for id in &suspects {
            let (fp, e, fw) = detail.get(id).copied().unwrap_or((0, 0.0, 0.0));
            if fp != 0 {
                tx.execute("UPDATE face SET suspect=1, guessed=?2, score_own=?3, score_guessed=?4 WHERE id=?1 AND local<>1", params![id, fp, e, fw]).map_err(error)?;
            }
        }
        tx.commit().map_err(error)?;
        text += &format!("{} Gesichter als verdächtig markiert.\n", suspects.len());
    }
    Ok(text)
}


/// Crop of a face (box slightly enlarged) from the photo, square at `side` pixels.
fn face_crop(db: &crate::db::Db, place: &HashMap<i64, (std::path::PathBuf, String)>, p: &Probe, side: u32) -> Option<image::RgbImage> {
    let (x, y, b, h): (f64, f64, f64, f64) = db.conn.query_row("SELECT x, y, w, h FROM face WHERE id=?1", [p.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).ok()?;
    let _ = place.get(&p.image)?;
    let image = crate::face_frame::of(db, p.image).and_then(|f| crate::face_frame::load(&f, 1600)).ok()?;
    let (w, hh) = (image.width() as f64, image.height() as f64);
    let (cx, cy, side_len) = ((x + b / 2.0) * w, (y + h / 2.0) * hh, (b * w).max(h * hh) * 1.3);
    let (x0, y0) = ((cx - side_len / 2.0).max(0.0), (cy - side_len / 2.0).max(0.0));
    let a = image::imageops::crop_imm(&image, x0 as u32, y0 as u32, (side_len as u32).min(image.width() - x0 as u32).max(1), (side_len as u32).min(image.height() - y0 as u32).max(1)).to_image();
    Some(image::imageops::resize(&a, side, side, image::imageops::FilterType::Triangle))
}

/// Sample sheet for a person: row 1 certain faces (references), row 2 suspicious names, row 3 suggestions for
/// faces without names (with similarity). Check helper and demonstration.
pub fn person_sheet(db: &crate::db::Db, name: &str, output: &std::path::Path) -> Result<String, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let person: i64 = db.conn.query_row("SELECT id FROM keyword WHERE person=1 AND name=?1", [name], |r| r.get(0)).map_err(|_| format!("Person „{name}“ nicht gefunden"))?;
    let all: Vec<Probe> = {
        let mut a = load_samples(&db.conn)?;
        // keep the state "suspect" for display
        a.retain(|p| p.embedding.len() > 1);
        a
    };
    let refs: Vec<&Probe> = all.iter().filter(|p| p.status == "confirmed" && p.person.is_some() && p.quality >= 0.25).collect();
    let mut safe: Vec<&Probe> = refs.iter().copied().filter(|p| p.person == Some(person)).collect();
    safe.sort_by(|a, b| b.quality.total_cmp(&a.quality));
    let mut suspicious: Vec<&Probe> = all.iter().filter(|p| p.status == "suspect" && p.person == Some(person)).collect();
    suspicious.sort_by(|a, b| b.quality.total_cmp(&a.quality));
    // suggestions: stored suggestions for this person (see `assign`), most certain first
    let stored = suggestion_list(&db.conn, Some(person), 0.0, 12, 0);
    let suggestions: Vec<(&Probe, f32, f32)> = stored.iter().filter_map(|v| all.iter().find(|p| p.id == v.face).map(|p| (p, v.confidence, v.second_item))).collect();
    let rows = db.rows().map_err(fe)?;
    let paths = db.folder_paths().map_err(fe)?;
    let place: HashMap<i64, (std::path::PathBuf, String)> = rows
        .iter()
        .filter(|z| z.copy.is_none())
        .filter_map(|z| {
            paths.get(&z.folder).map(|d| (z.image, (std::path::Path::new(d).join(&z.name), z.format.clone())))
        })
        .collect();
    let (side, columns) = (128u32, 12u32);
    let blocks: [(&str, Vec<&Probe>); 3] = [
        ("sicher", safe.iter().step_by((safe.len() / (columns as usize)).max(1)).take(columns as usize).copied().collect()),
        ("verdächtig", suspicious.iter().take(columns as usize).copied().collect()),
        ("Vorschlag", suggestions.iter().take(columns as usize).map(|v| v.0).collect()),
    ];
    let height = (side + 6) * 3;
    let mut arc = image::RgbImage::from_pixel(columns * (side + 4), height, image::Rgb([20, 20, 22]));
    let mut text = format!("{name}: {} bestätigte Vorbilder, {} verdächtig, {} Vorschläge gezeigt (von {} insgesamt)\n", safe.len(), suspicious.len(), suggestions.len(), suggestion_list(&db.conn, Some(person), 0.0, usize::MAX, 0).len());
    for (zi, (title, list)) in blocks.iter().enumerate() {
        text += &format!("Zeile {} = {title}\n", zi + 1);
        for (ci, p) in list.iter().enumerate() {
            if let Some(a) = face_crop(db, &place, p, side) {
                image::imageops::replace(&mut arc, &a, (ci as u32 * (side + 4)) as i64, (zi as u32 * (side + 6)) as i64);
            }
        }
    }
    for (p, w, z) in suggestions.iter().take(columns as usize) {
        let _ = p;
        text += &format!("  Vorschlag: Ähnlichkeit {w:.2}, zweitbeste Person {z:.2}\n");
    }
    arc.save(output).map_err(|e| e.to_string())?;
    Ok(text)
}

// ───────────────────────────────────────────── Checking names (a human decides)

/// A suspicious face for the check view.
#[derive(Clone, Debug)]
pub struct Review {
    pub face: i64,
    pub image: i64,
    pub person: Option<i64>,
    pub name: String,
    pub suggested: i64,
    pub suggested_name: String,
    pub own_value: f32,
    pub suggested_value: f32,
    pub quality: f32,
    pub file_name_of: String,
}

/// Number of faces whose name a human should check.
pub fn check_count(c: &Connection) -> usize {
    c.query_row("SELECT count(*) FROM face WHERE suspect=1 AND guessed IS NOT NULL AND local<>1", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
}

/// People with something to check: (keyword, name, count), by name.
pub fn test_people(c: &Connection) -> Vec<(i64, String, usize)> {
    let Ok(mut st) = c.prepare(
        "SELECT s.id, s.name, count(*) FROM face g JOIN keyword s ON s.id = g.person
         WHERE g.suspect=1 AND g.guessed IS NOT NULL AND g.local<>1 GROUP BY s.id ORDER BY lower(s.name), s.id",
    ) else {
        return Vec::new();
    };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as usize))).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Suspicious faces, largest deviation first. `person`: only faces with this name.
pub fn check_list(c: &Connection, person: Option<i64>, limit: usize, offset: usize) -> Vec<Review> {
    let Ok(mut st) = c.prepare(
        "SELECT g.id, g.image, g.person, coalesce(s.name,''), g.guessed, coalesce(v.name,''), coalesce(g.score_own,0), coalesce(g.score_guessed,0), coalesce(g.quality,0), coalesce(d.name,'')
         FROM face g LEFT JOIN keyword s ON s.id = g.person LEFT JOIN keyword v ON v.id = g.guessed
         LEFT JOIN image b ON b.id = g.image LEFT JOIN file d ON d.id = b.file
         WHERE g.suspect=1 AND g.guessed IS NOT NULL AND g.local<>1 AND (?1 IS NULL OR g.person = ?1)
         ORDER BY (coalesce(g.score_guessed,0) - coalesce(g.score_own,0)) DESC, g.quality DESC, g.id LIMIT ?2 OFFSET ?3",
    ) else {
        return Vec::new();
    };
    st.query_map(params![person, limit as i64, offset as i64], |r| {
        Ok(Review {
            face: r.get(0)?,
            image: r.get(1)?,
            person: r.get(2)?,
            name: r.get(3)?,
            suggested: r.get(4)?,
            suggested_name: r.get(5)?,
            own_value: r.get::<_, f64>(6)? as f32,
            suggested_value: r.get::<_, f64>(7)? as f32,
            quality: r.get::<_, f64>(8)? as f32,
            file_name_of: r.get(9)?,
        })
    })
    .map(|r| r.flatten().collect())
    .unwrap_or_default()
}

/// A human's answer to a suspicious name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Answer {
    /// The current name is correct.
    NameCorrect,
    /// The suspected name is correct (the current name is replaced).
    Correct,
    /// It is neither: remove the name, the face stays without a name.
    RemoveName,
}

/// Change the person on an image: the old keyword is dropped if no other face of the image carries this name any more; the new one is added.
/// Also writes the sidecar (the source of truth).
fn change_image_person(db: &crate::db::Db, image: i64, except_face: i64, old: Option<i64>, new: Option<i64>) -> Result<(), String> {
    let fe = |e: rusqlite::Error| e.to_string();
    // by id: a name is never taken apart into a path ("AC/DC", "A > B")
    let mut ids = crate::keywords::ids_of(&db.conn, image)?;
    let before = ids.clone();
    if let Some(a) = old {
        let remaining: i64 = db.conn.query_row("SELECT count(*) FROM face WHERE image=?1 AND person=?2 AND id<>?3 AND status='confirmed'", params![image, a, except_face], |r| r.get(0)).map_err(fe)?;
        if remaining == 0 {
            ids.retain(|x| *x != a);
        }
    }
    if let Some(n) = new {
        if !ids.contains(&n) {
            ids.push(n);
        }
    }
    if ids != before {
        let file: i64 = db.conn.query_row("SELECT file FROM image WHERE id=?1", [image], |r| r.get(0)).map_err(fe)?;
        crate::location::note_before_change(db, &[file]);
        crate::keywords::set_ids(db, image, &ids, crate::now(), true)?;
        // if the location is currently unreachable (NAS), the write waits in the queue (`crate::location`)
        let _ = crate::sync::write_sidecar(db, file);
    }
    Ok(())
}

/// Apply the answer: afterwards the face counts as decided by a human (`local = 1`); a Lightroom import and the
/// automatic check no longer change it. Excluded names are never suggested again.
pub fn apply_answer(db: &crate::db::Db, face: i64, answer: Answer) -> Result<(), String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let (image, person, suggested): (i64, Option<i64>, Option<i64>) =
        db.conn.query_row("SELECT image, person, guessed FROM face WHERE id=?1", [face], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(fe)?;
    match answer {
        Answer::NameCorrect => {
            db.conn.execute("UPDATE face SET suspect=0, local=1, status='confirmed' WHERE id=?1", [face]).map_err(fe)?;
            if let Some(v) = suggested {
                db.conn.execute("INSERT OR IGNORE INTO face_never(face, person) VALUES(?1, ?2)", params![face, v]).map_err(fe)?;
            }
        }
        Answer::Correct => {
            let new = suggested.ok_or("keine vermutete Person")?;
            db.conn.execute("UPDATE face SET person=?2, suspect=0, local=1, status='confirmed', guessed=NULL WHERE id=?1", params![face, new]).map_err(fe)?;
            if let Some(old) = person {
                db.conn.execute("INSERT OR IGNORE INTO face_never(face, person) VALUES(?1, ?2)", params![face, old]).map_err(fe)?;
            }
            change_image_person(db, image, face, person, Some(new))?;
        }
        Answer::RemoveName => {
            db.conn.execute("UPDATE face SET person=NULL, suspect=0, local=1, status='unknown', guessed=NULL WHERE id=?1", [face]).map_err(fe)?;
            for p in person.iter().chain(suggested.iter()) {
                db.conn.execute("INSERT OR IGNORE INTO face_never(face, person) VALUES(?1, ?2)", params![face, p]).map_err(fe)?;
            }
            change_image_person(db, image, face, person, None)?;
        }
    }
    Ok(())
}

/// Comparison form of a name for selection while typing (lower case, without accents).
pub fn person_brief(name: &str) -> String {
    crate::search::fold_text(name.trim())
}

/// All people (keywords with `person = 1`), sorted by name.
pub fn person_names(c: &Connection) -> Vec<(i64, String)> {
    let Ok(mut st) = c.prepare("SELECT id, name FROM keyword WHERE person=1 ORDER BY lower(name)") else { return Vec::new() };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Find the person with this name (case-insensitive) or create it (keyword, marked as person).
pub fn find_or_create_person(db: &crate::db::Db, name: &str) -> Result<i64, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let name = name.trim();
    if name.is_empty() {
        return Err("Kein Name angegeben".into());
    }
    if let Some(id) = db.conn.query_row("SELECT id FROM keyword WHERE person=1 AND lower(name)=lower(?1) ORDER BY id LIMIT 1", [name], |r| r.get::<_, i64>(0)).optional().map_err(fe)? {
        return Ok(id);
    }
    // the name is taken literally at the top level (no path splitting); a keyword of that name there becomes the person
    let id = crate::keywords::child(&db.conn, None, name)?;
    db.conn.execute("UPDATE keyword SET person=1 WHERE id=?1", [id]).map_err(fe)?;
    Ok(id)
}

/// Rename a person: if the new name already exists, both are merged. All faces, guesses and
/// exclusions of the old person move over to the new one; the keywords of the affected photos are swapped (the next sync
/// writes the sidecars; the database is newer then). Returns: number of reassigned faces.
pub fn rename_person(db: &crate::db::Db, from: i64, name: &str) -> Result<usize, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let new = find_or_create_person(db, name)?;
    if new == from {
        // only a different spelling of the same keyword: replace the name
        let n = name.trim();
        db.conn.execute("UPDATE keyword SET name=?2 WHERE id=?1", params![from, n]).map_err(fe)?;
        return Ok(0);
    }
    let images: Vec<i64> = {
        let mut st = db.conn.prepare("SELECT DISTINCT image FROM face WHERE person=?1").map_err(fe)?;
        let r = st.query_map([from], |r| r.get(0)).map_err(fe)?.collect::<rusqlite::Result<_>>().map_err(fe)?;
        r
    };
    let n = db.conn.execute("UPDATE face SET person=?2 WHERE person=?1", params![from, new]).map_err(fe)?;
    db.conn.execute("UPDATE face SET guessed=?2 WHERE guessed=?1", params![from, new]).map_err(fe)?;
    db.conn.execute("INSERT OR IGNORE INTO face_never(face, person) SELECT face, ?2 FROM face_never WHERE person=?1", params![from, new]).map_err(fe)?;
    db.conn.execute("DELETE FROM face_never WHERE person=?1", [from]).map_err(fe)?;
    // if the old person was hidden and the new one did not exist, it stays hidden
    db.conn.execute("INSERT OR IGNORE INTO person_info(keyword, hidden) SELECT ?2, hidden FROM person_info WHERE keyword=?1 AND hidden=1", params![from, new]).map_err(fe)?;
    for image in images {
        change_image_person_without_file(db, image, Some(from), Some(new))?;
    }
    Ok(n)
}

/// Like `change_image_person`, but does not write the sidecar right away (bulk rename: the sync catches up).
pub(crate) fn change_image_person_without_file(db: &crate::db::Db, image: i64, old: Option<i64>, new: Option<i64>) -> Result<(), String> {
    let mut ids = crate::keywords::ids_of(&db.conn, image)?;
    let before = ids.clone();
    if let Some(a) = old {
        let remaining: i64 = db.conn.query_row("SELECT count(*) FROM face WHERE image=?1 AND person=?2", params![image, a], |r| r.get(0)).map_err(|e| e.to_string())?;
        if remaining == 0 {
            ids.retain(|x| *x != a);
        }
    }
    if let Some(n) = new {
        if !ids.contains(&n) {
            ids.push(n);
        }
    }
    if ids != before {
        crate::keywords::set_ids(db, image, &ids, crate::now(), true)?;
    }
    Ok(())
}

/// Like [`Answer::Correct`], but with a freely entered name (existing or new person).
pub fn apply_answer_with_name(db: &crate::db::Db, face: i64, name: &str) -> Result<(), String> {
    let id = find_or_create_person(db, name)?;
    db.conn.execute("UPDATE face SET guessed=?2 WHERE id=?1", params![face, id]).map_err(|e| e.to_string())?;
    apply_answer(db, face, Answer::Correct)
}

/// Box drawn by hand (normalized to the upright image): creates a face without a name. The network first searches the
/// crop for more precise points; if it finds nothing, the points follow the box. Returns: face ID.
pub fn manual_face(db: &crate::db::Db, image: i64, x: f32, y: f32, b: f32, h: f32) -> Result<i64, String> {
    let model = load_model()?;
    let img = crate::face_frame::load(&crate::face_frame::of(db, image)?, search_edge())?;
    let (w, hh) = (img.width() as f32, img.height() as f32);
    let (px, py, pb, ph) = ((x * w).clamp(0.0, w - 1.0), (y * hh).clamp(0.0, hh - 1.0), (b * w).min(w), (h * hh).min(hh));
    if pb < 8.0 || ph < 8.0 {
        return Err("Rahmen zu klein".into());
    }
    let side_len = pb.max(ph) * 1.8;
    let (cx, cy) = (px + pb / 2.0, py + ph / 2.0);
    let (x0, y0) = ((cx - side_len / 2.0).max(0.0).floor(), (cy - side_len / 2.0).max(0.0).floor());
    let (cw, ch) = (side_len.min(w - x0).max(1.0), side_len.min(hh - y0).max(1.0));
    let crop = image::imageops::crop_imm(&img, x0 as u32, y0 as u32, cw as u32, ch as u32).to_image();
    let found = faces::detector::detect(&crop)
        .ok()
        .and_then(|v| {
            v.into_iter()
                .filter(|g| {
                    let (mx, my) = (g.x + g.b / 2.0 + x0, g.y + g.h / 2.0 + y0);
                    mx >= px && mx <= px + pb && my >= py && my <= py + ph
                })
                .max_by(|a, c| a.confidence.total_cmp(&c.confidence))
        })
        .and_then(|g| g.points);
    let points_list: [(f32, f32); 5] = match found {
        Some(p) => p.map(|q| (q.0 + x0, q.1 + y0)),
        None => faces::align::points_from_box(px, py, pb, ph),
    };
    let aligned = faces::align::crop_aligned(&img, &points_list);
    let embedding = model.embedding(&aligned)?;
    let quality = (pb * ph).sqrt() / 90.0;
    let quality = quality.min(1.0) * (0.4 + 0.6 * sharpness(&aligned));
    let mut raw_refs = [0f32; 10];
    for (i, q) in points_list.iter().enumerate() {
        raw_refs[2 * i] = q.0 / w;
        raw_refs[2 * i + 1] = q.1 / hh;
    }
    let points_blob = blob(&raw_refs);
    db.conn
        .execute(
            "INSERT INTO face(image, x, y, w, h, landmarks, embedding, model, quality, source, status, local, detected) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'manual','unknown',1,?10)",
            params![image, x.clamp(0.0, 1.0), y.clamp(0.0, 1.0), b.min(1.0), h.min(1.0), points_blob, blob(&embedding), model_name(), quality, crate::now()],
        )
        .map_err(|e| e.to_string())?;
    Ok(db.conn.last_insert_rowid())
}

/// Discard a face drawn by hand (only without a name; other faces stay).
pub fn discard_manual_face(db: &crate::db::Db, face: i64) -> Result<(), String> {
    db.conn.execute("DELETE FROM face WHERE id=?1 AND source='manual' AND person IS NULL", [face]).map_err(|e| e.to_string())?;
    Ok(())
}

/// Crop of a face as a JPEG file (box slightly enlarged, square) – cache for the check view.
pub fn write_face_image(db: &crate::db::Db, face: i64, target: &std::path::Path, side: u32) -> Result<(), String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let (image, x, y, b, h): (i64, f64, f64, f64, f64) = db.conn.query_row("SELECT image, x, y, w, h FROM face WHERE id=?1", [face], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map_err(fe)?;
    let img = crate::face_frame::load(&crate::face_frame::of(db, image)?, 1600)?;
    let (w, hh) = (img.width() as f64, img.height() as f64);
    let (cx, cy, side_len) = ((x + b / 2.0) * w, (y + h / 2.0) * hh, (b * w).max(h * hh) * 1.35);
    let (x0, y0) = ((cx - side_len / 2.0).max(0.0), (cy - side_len / 2.0).max(0.0));
    let crop = image::imageops::crop_imm(&img, x0 as u32, y0 as u32, (side_len as u32).min(img.width() - x0 as u32).max(1), (side_len as u32).min(img.height() - y0 as u32).max(1)).to_image();
    let small = image::imageops::resize(&crop, side, side, image::imageops::FilterType::Triangle);
    if let Some(d) = target.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let part = target.with_extension("teil");
    small.save_with_format(&part, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
    std::fs::rename(&part, target).map_err(|e| e.to_string())
}

/// File and format holding the pixels of an image.
pub fn source_to_image(db: &crate::db::Db, image: i64) -> Result<(std::path::PathBuf, String), String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let (format, folder, name): (String, i64, String) = db
        .conn
        .query_row("SELECT d.format, d.folder, d.name FROM image b JOIN file d ON d.id=b.file WHERE b.id=?1", [image], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(fe)?;
    let paths = db.folder_paths().map_err(fe)?;
    let dir = paths.get(&folder).ok_or("Ordner unbekannt")?;
    Ok((std::path::Path::new(dir).join(name), format))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    /// A face found while the photo stood rotated (BC) keeps its box in that frame in the catalog; the photo now stands
    /// normally (AB): the box is turned back 90° (checked on a catalog photo: own search and Lightroom agree then).
    #[test]
    fn lightroom_box_follows_a_later_rotation() {
        let c = [Some(0.3), Some(0.786), Some(0.342), Some(0.816)];
        assert_eq!(turn_lightroom_box(c, Some("AB"), Some("AB")), c);
        assert_eq!(turn_lightroom_box(c, None, Some("BC")), c);
        let t = turn_lightroom_box(c, Some("BC"), Some("AB")).map(|v| v.unwrap());
        // quarter turns 3: (x, y, b, h) → (y, 1 − x − b, h, b)
        let e = [0.786, 1.0 - 0.342, 0.816, 1.0 - 0.3];
        assert!(t.iter().zip(e).all(|(a, b)| (a - b).abs() < 1e-9), "{t:?}");
        let back = turn_lightroom_box(t.map(Some), Some("AB"), Some("BC")).map(|v| v.unwrap());
        assert!(back.iter().zip([0.3, 0.786, 0.342, 0.816]).all(|(a, b)| (a - b).abs() < 1e-9), "{back:?}");
    }

    /// Small library: two people with 12 faces each, plus a face of A that looks like B.
    fn test_library() -> (Db, i64, i64, i64) {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!("graineo-personen-{}-{}.sqlite", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_file(&p);
        let db = Db::open(&p).unwrap();
        let c = &db.conn;
        c.execute_batch("INSERT INTO root_folder(id, path, name, origin) VALUES(1,'/x/','x','own'); INSERT INTO folder(id, root, path) VALUES(1,1,'');").unwrap();
        c.execute("INSERT INTO keyword(id, name, person) VALUES(1,'Anna',1),(2,'Ben',1)", []).unwrap();
        // two directions in 8-dimensional space, small deviations
        let make_vec = |main_axis: usize, n: usize| -> Vec<f32> {
            let mut v = vec![0.05f32; 8];
            v[main_axis] = 1.0;
            v[(main_axis + 1 + n % 5) % 8] += 0.1 * (n as f32 % 3.0);
            let l = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter().map(|x| x / l).collect()
        };
        let mut id = 0;
        let mut image = |name: &str| -> i64 {
            id += 1;
            c.execute("INSERT INTO file(id, folder, name, format) VALUES(?1, 1, ?2, 'JPG')", params![id, name]).unwrap();
            c.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id]).unwrap();
            id
        };
        let mut face = |image: i64, person: i64, v: &[f32]| -> i64 {
            c.execute(
                "INSERT INTO face(image, x, y, w, h, embedding, model, quality, source, person, status) VALUES(?1,0.1,0.1,0.3,0.3,?2,'test',0.9,'lightroom',?3,'confirmed')",
                params![image, blob(v), person],
            )
            .unwrap();
            c.last_insert_rowid()
        };
        for n in 0..12 {
            let b = image(&format!("a{n}.jpg"));
            face(b, 1, &make_vec(0, n));
            let b = image(&format!("b{n}.jpg"));
            face(b, 2, &make_vec(4, n));
        }
        // wrongly named: looks like Ben, is called Anna
        let b = image("falsch.jpg");
        let wrong = face(b, 1, &make_vec(4, 2));
        (db, wrong, 1, 2)
    }

    /// Suggestions and groups are computed anew on every run, but a second run without changes writes no row (every
    /// face row holds its feature vector; the former reset-and-set wrote all of them each time), and the result stays.
    #[test]
    fn assign_writes_only_changed_rows() {
        let (db, _, anna, _) = test_library();
        let c = &db.conn;
        let unit = |axis: usize, n: usize| -> Vec<f32> {
            let mut v = vec![0.05f32; 8];
            v[axis] = 1.0;
            v[(axis + 1 + n % 3) % 8] += 0.05 * n as f32;
            let l = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter().map(|x| x / l).collect()
        };
        // unnamed faces: three like Anna (suggestions), four like nobody known (a group)
        for (n, axis) in [(0, 0), (1, 0), (2, 0), (0, 2), (1, 2), (2, 2), (3, 2)] {
            let id: i64 = c.query_row("SELECT coalesce(max(id),0)+1 FROM file", [], |r| r.get(0)).unwrap();
            c.execute("INSERT INTO file(id, folder, name, format) VALUES(?1, 1, ?2, 'JPG')", params![id, format!("u{id}.jpg")]).unwrap();
            c.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id]).unwrap();
            c.execute("INSERT INTO face(image, x, y, w, h, embedding, model, quality, source) VALUES(?1,0.1,0.1,0.3,0.3,?2,'test',0.9,'graineo')", params![id, blob(&unit(axis, n))]).unwrap();
        }
        let state = || -> Vec<(i64, Option<i64>, String, Option<i64>)> {
            let mut st = c.prepare("SELECT id, person, status, cluster FROM face ORDER BY id").unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect()
        };
        let rows: i64 = c.query_row("SELECT count(*) FROM face", [], |r| r.get(0)).unwrap();
        let t0 = c.total_changes();
        let first = assign(c, &Thresholds::default()).unwrap();
        let written_first = c.total_changes() - t0;
        let after_first = state();
        assert!(first.suggestions >= 3, "{first:?}");
        assert!(after_first.iter().filter(|z| z.2 == "suggested").all(|z| z.1 == Some(anna)));
        assert!(first.groups >= 1, "{first:?}");
        let t1 = c.total_changes();
        let second = assign(c, &Thresholds::default()).unwrap();
        let written_second = c.total_changes() - t1;
        assert_eq!(state(), after_first);
        assert_eq!((second.suggestions, second.groups), (first.suggestions, first.groups));
        assert_eq!(written_second, 0, "a run without changes wrote rows");
        // formerly every run wrote all rows once more (groups reset) plus the suggestions twice
        eprintln!("MEASUREMENT assign: {rows} faces, rows written: first run {written_first}, second run {written_second} (formerly ≥ {} per run)", rows as usize + 2 * first.suggestions);
        // a confirmed name changes the picture: the former suggestion of that face is no longer written back
        let some_suggestion = after_first.iter().find(|z| z.2 == "suggested").unwrap().0;
        c.execute("UPDATE face SET status='confirmed', local=1 WHERE id=?1", [some_suggestion]).unwrap();
        assign(c, &Thresholds::default()).unwrap();
        let z = state().into_iter().find(|z| z.0 == some_suggestion).unwrap();
        assert_eq!((z.1, z.2.as_str()), (Some(anna), "confirmed"));
    }

    /// New photos only: their unnamed faces get suggestions in one pass (without the full run).
    #[test]
    fn suggestions_for_new_photos_in_one_pass() {
        let (db, _, anna, ben) = test_library();
        let c = &db.conn;
        let unit = |axis: usize| -> Vec<f32> {
            let mut v = vec![0.05f32; 8];
            v[axis] = 1.0;
            let l = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter().map(|x| x / l).collect()
        };
        let mut images = Vec::new();
        for (n, axis) in [(0, 0), (1, 4)] {
            let id: i64 = c.query_row("SELECT coalesce(max(id),0)+1 FROM file", [], |r| r.get(0)).unwrap();
            c.execute("INSERT INTO file(id, folder, name, format) VALUES(?1, 1, ?2, 'JPG')", params![id, format!("neu{n}.jpg")]).unwrap();
            c.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id]).unwrap();
            c.execute("INSERT INTO face(image, x, y, w, h, embedding, model, quality, source) VALUES(?1,0.1,0.1,0.3,0.3,?2,'test',0.9,'graineo')", params![id, blob(&unit(axis))]).unwrap();
            images.push(id);
        }
        assert_eq!(suggestions_for_images(c, &images, &Thresholds::default()).unwrap(), 2);
        let who = |image: i64| c.query_row("SELECT person FROM face WHERE image=?1 AND status='suggested'", [image], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!((who(images[0]), who(images[1])), (anna, ben));
    }

    #[test]
    fn rename_lifts_name_and_merges() {
        let (db, _, anna, ben) = test_library();
        let c = &db.conn;
        let count_rows = |p: i64| c.query_row("SELECT count(*) FROM face WHERE person=?1", [p], |r| r.get::<_, i64>(0)).unwrap();
        // new name: Anna becomes Antonia (new person, all 13 faces move)
        let n = rename_person(&db, anna, "Antonia").unwrap();
        assert_eq!(n, 13);
        let antonia: i64 = c.query_row("SELECT id FROM keyword WHERE name='Antonia'", [], |r| r.get(0)).unwrap();
        assert_eq!((count_rows(anna), count_rows(antonia)), (0, 13));
        // existing name: Antonia merges into Ben
        rename_person(&db, antonia, "ben").unwrap();
        assert_eq!((count_rows(antonia), count_rows(ben)), (0, 25));
        // the photos' keyword was updated: no photo carries "Antonia" any more
        let b: i64 = c.query_row("SELECT image FROM face WHERE person=?1 LIMIT 1", [ben], |r| r.get(0)).unwrap();
        let paths = crate::sync::keyword_paths(&db, b).unwrap();
        assert!(paths.iter().any(|p| p == "Ben") && !paths.iter().any(|p| p == "Antonia"), "{paths:?}");
    }

    #[test]
    fn misnamed_face_is_detected_and_answered() {
        let (db, wrong, anna, ben) = test_library();
        let report = clean_up(&db.conn, 0.15, 0.45, true).unwrap();
        assert!(report.contains("verdächtig"), "{report}");
        assert_eq!(check_count(&db.conn), 1, "{report}");
        let list = check_list(&db.conn, None, 10, 0);
        assert_eq!((list[0].face, list[0].person, list[0].suggested), (wrong, Some(anna), ben));
        assert_eq!(test_people(&db.conn).len(), 1);
        // the suspicious face does not count as a reference
        let samples = load_samples(&db.conn).unwrap();
        assert_eq!(samples.iter().find(|p| p.id == wrong).unwrap().status, "suspect");

        // "name is correct": no longer suspicious, not marked again, Ben never suggested again
        apply_answer(&db, wrong, Answer::NameCorrect).unwrap();
        assert_eq!(check_count(&db.conn), 0);
        clean_up(&db.conn, 0.15, 0.45, true).unwrap();
        assert_eq!(check_count(&db.conn), 0, "confirmed by a human stays confirmed");
    }

    #[test]
    fn correct_sets_new_name_and_remembers_old() {
        let (db, wrong, anna, ben) = test_library();
        clean_up(&db.conn, 0.15, 0.45, true).unwrap();
        apply_answer(&db, wrong, Answer::Correct).unwrap();
        let (person, status, local): (i64, String, i64) = db.conn.query_row("SELECT person, status, local FROM face WHERE id=?1", [wrong], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((person, status.as_str(), local), (ben, "confirmed", 1));
        let never: i64 = db.conn.query_row("SELECT count(*) FROM face_never WHERE face=?1 AND person=?2", params![wrong, anna], |r| r.get(0)).unwrap();
        assert_eq!(never, 1);
        // keyword on the image: Anna gone, Ben added
        let image: i64 = db.conn.query_row("SELECT image FROM face WHERE id=?1", [wrong], |r| r.get(0)).unwrap();
        let paths = crate::sync::keyword_paths(&db, image).unwrap();
        assert_eq!(paths, vec!["Ben".to_string()]);
    }

    #[test]
    fn own_name_creates_person_or_reuses_existing() {
        let (db, wrong, _, ben) = test_library();
        clean_up(&db.conn, 0.15, 0.45, true).unwrap();
        apply_answer_with_name(&db, wrong, "Clara").unwrap();
        let (person, status): (i64, String) = db.conn.query_row("SELECT person, status FROM face WHERE id=?1", [wrong], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(status, "confirmed");
        let (name, is_person): (String, i64) = db.conn.query_row("SELECT name, person FROM keyword WHERE id=?1", [person], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((name.as_str(), is_person), ("Clara", 1));
        assert!(person_names(&db.conn).iter().any(|(_, n)| n == "Clara"));
        // an existing person (regardless of spelling) is reused
        assert_eq!(find_or_create_person(&db, "ben").unwrap(), ben);
        assert!(find_or_create_person(&db, "  ").is_err());
    }

    #[test]
    fn removing_name_leaves_face_unnamed() {
        let (db, wrong, _, _) = test_library();
        clean_up(&db.conn, 0.15, 0.45, true).unwrap();
        apply_answer(&db, wrong, Answer::RemoveName).unwrap();
        let (person, status): (Option<i64>, String) = db.conn.query_row("SELECT person, status FROM face WHERE id=?1", [wrong], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((person, status.as_str()), (None, "unknown"));
        assert_eq!(check_count(&db.conn), 0);
    }
}


/// Measuring the detection rate: how many of the confirmed faces does Vision find (whole image, tile grid `n`×`n` with fraction)?
/// `images`: number of photos (evenly chosen from the photos with confirmed faces).
pub fn detection_test(db: &crate::db::Db, images: usize, n: usize, fraction: f32) -> Result<String, String> {
    use rayon::prelude::*;
    let fe = |e: rusqlite::Error| e.to_string();
    let all: Vec<i64> = {
        let mut st = db.conn.prepare("SELECT DISTINCT image FROM face WHERE status='confirmed' ORDER BY image").map_err(fe)?;
        let r = st.query_map([], |r| r.get::<_, i64>(0)).map_err(fe)?.collect::<rusqlite::Result<_>>().map_err(fe)?;
        r
    };
    let step = (all.len() / images.max(1)).max(1);
    // fetch everything needed from the database up front (the connection is not meant for several threads)
    let mut tasks: Vec<(std::path::PathBuf, String, Vec<(f32, f32, f32, f32)>)> = Vec::new();
    for &image in all.iter().step_by(step).take(images) {
        let Ok((path, fm)) = source_to_image(db, image) else { continue };
        let mut st = db.conn.prepare("SELECT x, y, w, h FROM face WHERE image=?1 AND status='confirmed'").map_err(fe)?;
        let rect_list = st.query_map([image], |r| Ok((r.get::<_, f64>(0)? as f32, r.get::<_, f64>(1)? as f32, r.get::<_, f64>(2)? as f32, r.get::<_, f64>(3)? as f32))).map_err(fe)?.collect::<rusqlite::Result<Vec<_>>>().map_err(fe)?;
        tasks.push((path, fm, rect_list));
    }
    let t = std::time::Instant::now();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().map_err(|e| e.to_string())?;
    let outcomes: Vec<(usize, usize, usize)> = pool.install(|| {
        tasks
            .par_iter()
            .map(|(path, fm, rect_list)| {
                let Ok((img, _)) = crate::preview::load(path, fm, search_edge()) else { return (0, 0, 0) };
                let (w, h) = (img.width() as f32, img.height() as f32);
                let detected = if n <= 1 { faces::detector::detect(&img) } else { faces::detector::detect_tile_grid(&img, n, fraction) }.unwrap_or_default();
                let matches = rect_list.iter().filter(|r| detected.iter().any(|g| overlap((g.x / w, g.y / h, g.b / w, g.h / h), **r) > 0.45)).count();
                (rect_list.len(), matches, detected.len())
            })
            .collect()
    });
    let (expected, actual, detected): (usize, usize, usize) = outcomes.iter().fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
    Ok(format!(
        "Raster {n}×{n} (Anteil {fraction:.2}): {actual} von {expected} bestätigten Gesichtern gefunden ({:.1} %), insgesamt {detected} Gesichter, {} Fotos in {:?} ({:.0} ms je Foto, 4 Threads)\n",
        100.0 * actual as f64 / expected.max(1) as f64,
        tasks.len(),
        t.elapsed(),
        t.elapsed().as_millis() as f64 / tasks.len().max(1) as f64
    ))
}

/// Compute time only from what is free (macOS: QoS "utility"), so that UI and developing stay smooth.
pub fn low_priority() {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn pthread_set_qos_class_self_np(qos: u32, rel: i32) -> i32;
        }
        unsafe {
            pthread_set_qos_class_self_np(0x11, 0); // QOS_CLASS_UTILITY
        }
    }
}

// ───────────────────────────────────────────── Background run

/// Are one-time steps after an update still open ([`once_after_update`])? Reads three values (cheap).
pub fn once_pending(db: &crate::db::Db) -> bool {
    let catalog = db.info("lightroom_catalog").is_some_and(|p| std::path::Path::new(&p).exists());
    let lr = catalog && db.info("faces_lr").as_deref() != Some(LR_REVISION);
    let frame = db.info("face_frames").as_deref() != Some(crate::face_frame::REVISION) || db.info("face_frames_open").is_some_and(|s| s.trim() != "[]" && !s.trim().is_empty());
    lr || frame
}

/// The one-time steps after an update, before any face search: the frame repair of the own boxes
/// ([`crate::face_frame::repair`]), the face images that may show a wrong place
/// ([`crate::face_index::purge_outdated`]) and the update from the Lightroom catalog ([`lightroom_update`]).
/// Each step is done when its revision is stored; a quit in between only repeats what is not stored yet. The repair
/// comes first: it is what the person sees (boxes on the wrong spot), and it does not depend on the others.
/// Returns lines for the log.
pub fn once_after_update(db: &crate::db::Db, cancel: &std::sync::atomic::AtomicBool, report_fn: &(dyn Fn(String) + Sync)) -> Vec<String> {
    let mut notes = Vec::new();
    let t = std::time::Instant::now();
    match crate::face_frame::repair(db) {
        Ok(Some(r)) => notes.push(format!("faces: frame repaired in {:.1} s: {r:?}", t.elapsed().as_secs_f64())),
        Ok(None) => {}
        Err(e) => notes.push(format!("faces: frame repair failed: {e}")),
    }
    let purged = crate::face_index::purge_outdated(cancel);
    if purged > 0 {
        notes.push(format!("faces: {purged} outdated face images removed"));
    }
    if db.info("faces_lr").as_deref() != Some(LR_REVISION) {
        report_fn("Gesichter aus Lightroom …".into());
        let t = std::time::Instant::now();
        match lightroom_update(db) {
            Ok(Some((all, named))) => notes.push(format!("faces: Lightroom update (revision {LR_REVISION}) in {:.1} s: {all} faces, {named} named", t.elapsed().as_secs_f64())),
            Ok(None) => {}
            Err(e) => notes.push(format!("faces: Lightroom update failed: {e}")),
        }
    }
    notes
}

/// Find faces in all images not yet searched (Vision), compute features (SFace) and check the names
/// (suspicious names for "Namen prüfen"). Runs in batches; `cancel` ends it between batches. The model is
/// loaded the first time. `report_fn` receives status texts for the status line.
pub fn run(db: &crate::db::Db, cancel: &std::sync::atomic::AtomicBool, report_fn: &(dyn Fn(String) + Sync)) -> Result<SearchReport, String> {
    use std::sync::atomic::Ordering;
    // nobody waits for it: efficiency cores (its search pool runs there too)
    crate::platform::background_priority();
    let mut notes = once_after_update(db, cancel, report_fn);
    // photos whose search failed for a passing reason at the last run are open again
    if let Ok(n) = db.conn.execute("UPDATE image SET faces_scanned=0 WHERE faces_scanned=?1", [SEARCH_RETRY]) {
        if n > 0 {
            notes.push(format!("faces: {n} photos tried again"));
        }
    }
    let total = open_jobs(db, usize::MAX)?.len();
    // the model (39 MB) only when there are photos to search: not at the first start with an empty library
    let model = if total > 0 {
        report_fn("Gesichtsmodell …".into());
        Some(load_model()?)
    } else {
        None
    };
    // progress from the first photo on: a batch holds up to 600 photos and can take minutes on the efficiency
    // cores under load, while the status line still said "Gesichtsmodell …" (looked like a hanging model load)
    report_fn(format!("Gesichter: {} von {} Fotos", 0, total));
    let mut sums = SearchReport { notes, ..SearchReport::default() };
    let mut since_check = 0usize;
    let started = crate::now();
    while !cancel.load(Ordering::Relaxed) {
        let done = sums.images;
        let Some(model) = model.as_ref() else { break };
        let b = search(db, model, 600, 2, cancel, &|n, _| report_fn(format!("Gesichter: {} von {} Fotos", done + n, total)))?;
        sums.images += b.images;
        sums.error += b.error;
        sums.retry += b.retry;
        sums.faces += b.faces;
        sums.connected += b.connected;
        sums.new += b.new;
        since_check += b.images;
        report_fn(format!("Gesichter: {} von {} Fotos", sums.images, total));
        if since_check >= 4000 {
            since_check = 0;
            report_fn("Namen prüfen …".into());
            let _ = clean_up(&db.conn, 0.15, 0.45, true);
            report_fn("Namen vorschlagen …".into());
            let _ = assign(&db.conn, &Thresholds::default());
        }
        if b.images == 0 {
            break;
        }
        // a whole batch failed for passing reasons (Vision or the model not working right now): stop instead of
        // going through the whole library; the next run tries again
        if b.images >= 20 && b.retry == b.images {
            sums.notes.push(format!("faces: search stopped, {} photos in a row failed (tried again at the next run)", b.retry));
            break;
        }
    }
    // The full check and suggestion run (15–20 s of computing with 100 000 faces) only when what it depends on changed
    // (confirmed faces, rejected names, hidden people – also through this run). Formerly it ran at every program start
    // even without anything new. New photos alone only get suggestions for their own faces.
    if crate::people_state::assign_dirty(&db.conn) {
        report_fn("Namen prüfen …".into());
        let _ = clean_up(&db.conn, 0.15, 0.45, true);
        report_fn("Namen vorschlagen …".into());
        let _ = assign(&db.conn, &Thresholds::default());
    } else if since_check > 0 {
        report_fn("Namen vorschlagen …".into());
        let images: Vec<i64> = db
            .conn
            .prepare("SELECT DISTINCT image FROM face WHERE detected >= ?1 AND person IS NULL")
            .and_then(|mut st| st.query_map([started], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect()))
            .unwrap_or_default();
        let _ = suggestions_for_images(&db.conn, &images, &Thresholds::default());
    }
    // face images for the open questions and groups, ready before the people view shows them (mostly from the
    // preview cache: a few ms each)
    let missing: Vec<i64> = crate::face_index::missing_thumbs(&db.conn);
    if !missing.is_empty() {
        report_fn(format!("Gesichtsbilder: {} …", missing.len()));
        let _ = crate::face_index::prefill(db, &missing, cancel);
    }
    Ok(sums)
}

// ───────────────────────────────────────────── Suggestions and groups for faces without names

/// Result of [`assign`].
#[derive(Debug, Default, Clone)]
pub struct AssignReport {
    pub references: usize,
    pub queries: usize,
    pub suggestions: usize,
    pub backup_copy: usize,
    pub groups: usize,
    pub in_groups: usize,
}

/// People who should not be shown (get no suggestions, are not in the lists).
fn hidden_items(c: &Connection) -> std::collections::HashSet<i64> {
    let Ok(mut st) = c.prepare("SELECT keyword FROM person_info WHERE hidden=1") else { return Default::default() };
    st.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Suggest names for faces without names and group unnamed faces.
///
/// Rules (own definition, behavior modeled on the face recognition of common photo managers): the measure is the best single hit among the confirmed faces
/// of a person; two faces in the same photo are never the same person (the higher similarity wins); excluded
/// names (`face_never`) and hidden people are not suggested. Suggestions are not decisions
/// (`status = 'suggested'`); whatever a human confirmed or rejected stays untouched.
pub fn assign(c: &Connection, s: &Thresholds) -> Result<AssignReport, String> {
    // one run at a time: the background run after the face search and the refresh after a confirmation must not overlap
    static RUNNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _lock = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    // the state is noted before reading: a change during the run makes the next check find it again
    let state = crate::people_state::assign_signature(c);
    let report = assign_inner(c, s)?;
    // the suggestions from unclear Apple groups were reset with the others: set them again
    let _ = crate::apple_groups::apply_suggestions(c);
    crate::people_state::mark_assigned(c, &state);
    Ok(report)
}

fn assign_inner(c: &Connection, s: &Thresholds) -> Result<AssignReport, String> {
    let mut report = AssignReport::default();
    // Suggestions and groups are computed anew, but only rows whose values change are written: every face row holds
    // its feature vector, and rewriting all of them on each run (formerly: reset all, then set again) wrote hundreds of
    // megabytes into the database and its journal per run (seen: 2 GB in 15 minutes).
    let before = suggestion_state(c)?;
    let mut all = load_samples(c)?;
    // former suggestions count as open again (as if reset)
    for p in all.iter_mut().filter(|p| p.status == "suggested" && !p.local) {
        p.person = None;
        p.status = "unknown".into();
    }
    let hidden = hidden_items(c);
    let refs: Vec<&Probe> = all.iter().filter(|p| p.status == "confirmed" && p.person.is_some_and(|x| !hidden.contains(&x)) && p.quality >= s.min_quality).collect();
    report.references = refs.len();
    let queries: Vec<&Probe> = all.iter().filter(|p| p.person.is_none() && p.status != "rejected" && p.quality >= 0.3).collect();
    report.queries = queries.len();
    if refs.is_empty() || queries.is_empty() {
        write_suggestions(c, &before, &HashMap::new(), &HashMap::new())?;
        return Ok(report);
    }
    // per photo the already confirmed people: not again
    let mut never: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let mut st = c.prepare("SELECT face, person FROM face_never").map_err(error)?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(error)? {
            let (g, p) = r.map_err(error)?;
            never.entry(g).or_default().push(p);
        }
    }
    let mut in_image: HashMap<i64, Vec<i64>> = HashMap::new();
    for p in all.iter().filter(|p| p.status == "confirmed") {
        if let Some(pe) = p.person {
            in_image.entry(p.image).or_default().push(pe);
        }
    }
    for q in &queries {
        if let Some(v) = in_image.get(&q.image) {
            never.entry(q.id).or_default().extend(v.iter().copied());
        }
    }
    // at most two passes: losers of a photo conflict get the next-best person
    let mut open_set: Vec<&Probe> = queries.clone();
    let mut assignment: HashMap<i64, (i64, f32, f32)> = HashMap::new();
    for _ in 0..2 {
        let matches = best_person(&open_set, &refs, s.k, 0, &never);
        let mut per_image: HashMap<(i64, i64), Vec<(f32, usize)>> = HashMap::new();
        for (i, (q, t)) in open_set.iter().zip(&matches).enumerate() {
            if let Some(t) = t.as_ref().filter(|t| t.value >= s.suggestion && t.value - t.second_item >= s.distance * 0.5) {
                per_image.entry((q.image, t.person)).or_default().push((t.value, i));
            }
        }
        let mut loser: Vec<&Probe> = Vec::new();
        let mut winner: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (_, mut v) in per_image {
            v.sort_by(|a, b| b.0.total_cmp(&a.0));
            winner.insert(v[0].1);
            for (_, i) in v.iter().skip(1) {
                loser.push(open_set[*i]);
                if let Some(Some(t)) = matches.get(*i) {
                    never.entry(open_set[*i].id).or_default().push(t.person);
                }
            }
        }
        for (i, (q, t)) in open_set.iter().zip(&matches).enumerate() {
            if let Some(t) = t.as_ref().filter(|t| t.value >= s.suggestion && t.value - t.second_item >= s.distance * 0.5 && winner.contains(&i)) {
                assignment.insert(q.id, (t.person, t.value, t.second_item));
            }
        }
        if loser.is_empty() {
            break;
        }
        open_set = loser;
    }
    // only faces set in graineo stay as they are (as before: `lokal=0`)
    let local: std::collections::HashSet<i64> = all.iter().filter(|p| p.local).map(|p| p.id).collect();
    assignment.retain(|id, _| !local.contains(id));
    for (value, second_item) in assignment.values().map(|a| (a.1, a.2)) {
        report.suggestions += 1;
        report.backup_copy += usize::from(value >= s.safe && value - second_item >= s.distance);
    }
    // groups: faces without a name and without a suggestion
    let rest: Vec<&Probe> = queries.iter().copied().filter(|p| !assignment.contains_key(&p.id) && p.quality >= 0.4).collect();
    let members: Vec<Vec<i64>> = group_by(&rest, s.group).into_iter().map(|g| g.into_iter().map(|i| rest[i].id).collect()).collect();
    let mut groups: HashMap<i64, i64> = HashMap::new();
    for m in members.iter().filter(|m| m.len() >= 3) {
        let gid = *m.iter().min().unwrap();
        report.groups += 1;
        for id in m {
            groups.insert(*id, gid);
            report.in_groups += 1;
        }
    }
    write_suggestions(c, &before, &assignment, &groups)?;
    Ok(report)
}

/// Suggestion values of a face: person, certainty, own value, group.
type SuggestionRow = (Option<i64>, Option<f64>, Option<f64>, Option<i64>);

/// Faces that carry a suggestion (not set in graineo) or a group now: id → (suggestion values, is a suggestion).
fn suggestion_state(c: &Connection) -> Result<HashMap<i64, (SuggestionRow, bool)>, String> {
    let mut st = c
        .prepare("SELECT id, person, confidence, score_own, cluster, status='suggested' AND local=0 FROM face WHERE (status='suggested' AND local=0) OR cluster IS NOT NULL")
        .map_err(error)?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, ((r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?), r.get::<_, bool>(5)?))))
        .map_err(error)?
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .map_err(error)?;
    Ok(rows)
}

/// Write the new suggestions and groups: only faces whose values change (one transaction). Former suggestions without
/// a new one become "unknown" again, former groups are cleared.
fn write_suggestions(c: &Connection, before: &HashMap<i64, (SuggestionRow, bool)>, assignment: &HashMap<i64, (i64, f32, f32)>, groups: &HashMap<i64, i64>) -> Result<usize, String> {
    let tx = c.unchecked_transaction().map_err(error)?;
    let mut written = 0;
    let mut ids: Vec<i64> = before.keys().chain(assignment.keys()).chain(groups.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let old = before.get(&id);
        let new_group = groups.get(&id).copied();
        match assignment.get(&id) {
            Some(&(person, value, second)) => {
                let new: SuggestionRow = (Some(person), Some(value as f64), Some(second as f64), new_group);
                if old != Some(&(new, true)) {
                    tx.execute("UPDATE face SET person=?2, status='suggested', confidence=?3, score_own=?4, cluster=?5 WHERE id=?1 AND local=0", params![id, person, value, second, new_group]).map_err(error)?;
                    written += 1;
                }
            }
            None => {
                let was_suggestion = old.is_some_and(|o| o.1);
                let old_group = old.and_then(|o| o.0 .3);
                if was_suggestion {
                    tx.execute("UPDATE face SET person=NULL, status='unknown', confidence=NULL, score_own=NULL, cluster=?2 WHERE id=?1 AND status='suggested' AND local=0", params![id, new_group]).map_err(error)?;
                    written += 1;
                } else if old_group != new_group {
                    tx.execute("UPDATE face SET cluster=?2 WHERE id=?1", params![id, new_group]).map_err(error)?;
                    written += 1;
                }
            }
        }
    }
    tx.commit().map_err(error)?;
    Ok(written)
}

/// Search state of an image: 0 = not searched yet, 1 = searched, 2 = search failed (also when it is tried again at
/// the next run).
pub fn search_state(c: &Connection, image: i64) -> i64 {
    c.query_row("SELECT faces_scanned FROM image WHERE id=?1", [image], |r| r.get::<_, i64>(0)).map(|v| if v == SEARCH_RETRY { 2 } else { v }).unwrap_or(1)
}

/// Search a single photo right away (for the single view, if the background run has not reached it yet) and suggest
/// names for the faces found. Returns the number of faces in the photo.
pub fn check_image(db: &crate::db::Db, image: i64) -> Result<usize, String> {
    let state: i64 = db.conn.query_row("SELECT faces_scanned FROM image WHERE id=?1", [image], |r| r.get(0)).unwrap_or(1);
    if state == 0 || state == SEARCH_RETRY {
        let job = Job::of(db, image)?;
        let model = load_model()?;
        let cancel = std::sync::atomic::AtomicBool::new(false);
        search_jobs(db, &model, vec![job], 1, &cancel, &|_, _| {})?;
        suggestions_for_image(&db.conn, image, &Thresholds::default())?;
    }
    Ok(faces_to_image(&db.conn, image).len())
}

/// Suggest names for the faces without a person in one photo (same rules as [`assign`], but only for this photo).
pub fn suggestions_for_image(c: &Connection, image: i64, s: &Thresholds) -> Result<usize, String> {
    suggestions_for_images(c, &[image], s)
}

/// Like [`suggestions_for_image`] for several photos (the features are read once).
pub fn suggestions_for_images(c: &Connection, images: &[i64], s: &Thresholds) -> Result<usize, String> {
    if images.is_empty() {
        return Ok(0);
    }
    let all = load_samples(c)?;
    let set: std::collections::HashSet<i64> = images.iter().copied().collect();
    let hidden = hidden_items(c);
    let refs: Vec<&Probe> = all.iter().filter(|p| p.status == "confirmed" && p.person.is_some_and(|x| !hidden.contains(&x)) && p.quality >= s.min_quality).collect();
    let queries: Vec<&Probe> = all.iter().filter(|p| set.contains(&p.image) && p.person.is_none() && p.status != "rejected" && p.quality >= 0.3).collect();
    if refs.is_empty() || queries.is_empty() {
        return Ok(0);
    }
    let mut never: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let mut st = c.prepare("SELECT face, person FROM face_never").map_err(error)?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(error)? {
            let (g, p) = r.map_err(error)?;
            never.entry(g).or_default().push(p);
        }
    }
    // whoever is already confirmed in the photo does not appear there again
    let mut already: HashMap<i64, Vec<i64>> = HashMap::new();
    for p in all.iter().filter(|p| set.contains(&p.image) && p.status == "confirmed") {
        if let Some(pe) = p.person {
            already.entry(p.image).or_default().push(pe);
        }
    }
    for q in &queries {
        if let Some(v) = already.get(&q.image) {
            never.entry(q.id).or_default().extend(v.iter().copied());
        }
    }
    let mut open_set: Vec<&Probe> = queries;
    let mut assignment: HashMap<i64, (i64, f32, f32)> = HashMap::new();
    for _ in 0..2 {
        let matches = best_person(&open_set, &refs, s.k, 0, &never);
        // the same person only once per photo: the higher similarity wins, the other looks for the next-best
        let mut per_person: HashMap<(i64, i64), Vec<(f32, usize)>> = HashMap::new();
        for (i, t) in matches.iter().enumerate() {
            if let Some(t) = t.as_ref().filter(|t| t.value >= s.suggestion && t.value - t.second_item >= s.distance * 0.5) {
                per_person.entry((open_set[i].image, t.person)).or_default().push((t.value, i));
            }
        }
        let mut loser: Vec<&Probe> = Vec::new();
        for ((_, person), mut v) in per_person {
            v.sort_by(|a, b| b.0.total_cmp(&a.0));
            if let Some(Some(t)) = matches.get(v[0].1) {
                assignment.insert(open_set[v[0].1].id, (person, t.value, t.second_item));
            }
            for (_, i) in v.iter().skip(1) {
                loser.push(open_set[*i]);
                never.entry(open_set[*i].id).or_default().push(person);
            }
        }
        if loser.is_empty() {
            break;
        }
        open_set = loser;
    }
    let tx = c.unchecked_transaction().map_err(error)?;
    for (id, (person, value, second_item)) in &assignment {
        tx.execute("UPDATE face SET person=?2, status='suggested', confidence=?3, score_own=?4 WHERE id=?1 AND local=0", params![id, person, value, second_item]).map_err(error)?;
    }
    tx.commit().map_err(error)?;
    Ok(assignment.len())
}

/// A suggestion for a face without a name.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub face: i64,
    pub image: i64,
    pub person: i64,
    pub name: String,
    pub confidence: f32,
    pub second_item: f32,
    pub quality: f32,
    pub file_name_of: String,
}

pub fn suggestion_count(c: &Connection) -> usize {
    c.query_row("SELECT count(*) FROM face g WHERE g.status='suggested' AND g.person IS NOT NULL AND NOT EXISTS (SELECT 1 FROM person_info p WHERE p.keyword=g.person AND p.hidden=1)", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
}

/// Suggestions, most certain first. `person`: only this name; `ab`: minimum similarity.
pub fn suggestion_list(c: &Connection, person: Option<i64>, ab: f32, limit: usize, offset: usize) -> Vec<Suggestion> {
    let Ok(mut st) = c.prepare(
        "SELECT g.id, g.image, g.person, coalesce(s.name,''), coalesce(g.confidence,0), coalesce(g.score_own,0), coalesce(g.quality,0), coalesce(d.name,'')
         FROM face g JOIN keyword s ON s.id=g.person LEFT JOIN image b ON b.id=g.image LEFT JOIN file d ON d.id=b.file
         WHERE g.status='suggested' AND (?1 IS NULL OR g.person=?1) AND coalesce(g.confidence,0) >= ?2
           AND NOT EXISTS (SELECT 1 FROM person_info p WHERE p.keyword=g.person AND p.hidden=1)
         ORDER BY (coalesce(g.confidence,0) - coalesce(g.score_own,0)) DESC, g.confidence DESC, g.id LIMIT ?3 OFFSET ?4",
    ) else {
        return Vec::new();
    };
    st.query_map(params![person, ab as f64, limit as i64, offset as i64], |r| {
        Ok(Suggestion { face: r.get(0)?, image: r.get(1)?, person: r.get(2)?, name: r.get(3)?, confidence: r.get::<_, f64>(4)? as f32, second_item: r.get::<_, f64>(5)? as f32, quality: r.get::<_, f64>(6)? as f32, file_name_of: r.get(7)? })
    })
    .map(|r| r.flatten().collect())
    .unwrap_or_default()
}

/// People with suggestions: (keyword, name, count), by name.
pub fn suggestion_people(c: &Connection) -> Vec<(i64, String, usize)> {
    let Ok(mut st) = c.prepare(
        "SELECT s.id, s.name, count(*) FROM face g JOIN keyword s ON s.id=g.person WHERE g.status='suggested'
           AND NOT EXISTS (SELECT 1 FROM person_info p WHERE p.keyword=g.person AND p.hidden=1) GROUP BY s.id ORDER BY lower(s.name), s.id",
    ) else {
        return Vec::new();
    };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as usize))).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Confirm (`yes`) or reject a suggestion. Confirmed: the face carries the name, is decided by a human and the photo
/// gets the person keyword. Rejected: the name is never suggested for this face again.
pub fn answer_suggestion(db: &crate::db::Db, face: i64, yes: bool) -> Result<(), String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let (image, person): (i64, Option<i64>) = db.conn.query_row("SELECT image, person FROM face WHERE id=?1", [face], |r| Ok((r.get(0)?, r.get(1)?))).map_err(fe)?;
    let Some(p) = person else { return Ok(()) };
    if yes {
        db.conn.execute("UPDATE face SET status='confirmed', local=1, suspect=0, guessed=NULL WHERE id=?1", [face]).map_err(fe)?;
        change_image_person(db, image, face, None, Some(p))
    } else {
        db.conn.execute("INSERT OR IGNORE INTO face_never(face, person) VALUES(?1, ?2)", params![face, p]).map_err(fe)?;
        db.conn.execute("UPDATE face SET person=NULL, status='unknown', confidence=NULL, score_own=NULL WHERE id=?1", [face]).map_err(fe)?;
        Ok(())
    }
}

/// Confirm all suggestions above a similarity and with a clear margin to the second-best person. Returns: count.
pub fn confirm_backups(db: &crate::db::Db, ab: f32, distance: f32) -> Result<usize, String> {
    confirm_backups_for(db, None, ab, distance)
}

/// Like [`confirm_backups`], optionally only for the suggestions of one person (keyword).
pub fn confirm_backups_for(db: &crate::db::Db, person: Option<i64>, ab: f32, distance: f32) -> Result<usize, String> {
    let ids: Vec<i64> = suggestion_list(&db.conn, person, ab, usize::MAX, 0).into_iter().filter(|v| v.confidence - v.second_item >= distance).map(|v| v.face).collect();
    for id in &ids {
        answer_suggestion(db, *id, true)?;
    }
    Ok(ids.len())
}

/// Group of unnamed faces that are similar to each other.
#[derive(Clone, Debug)]
pub struct Group {
    pub id: i64,
    pub count: usize,
    /// up to six faces (best quality first)
    pub examples: Vec<i64>,
}

pub fn group_count(c: &Connection) -> usize {
    // `+person`: the partial index on `cluster` instead of the person index (which would read all unnamed faces)
    c.query_row("SELECT count(DISTINCT cluster) FROM face WHERE cluster IS NOT NULL AND +person IS NULL AND status<>'rejected'", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
}

/// Groups, largest first, with up to six example faces (best quality first) – one query over the partial index
/// `face_cluster` (formerly one query per group over all unnamed faces: 10 s with 400 groups and 110 000 faces).
pub fn group_list(c: &Connection, limit: usize, offset: usize) -> Vec<Group> {
    let Ok(mut st) = c.prepare(
        "WITH g AS (SELECT cluster, count(*) AS n FROM face WHERE cluster IS NOT NULL AND +person IS NULL AND status<>'rejected'
                    GROUP BY cluster ORDER BY n DESC, cluster LIMIT ?1 OFFSET ?2)
         SELECT g.cluster, g.n, (SELECT group_concat(id) FROM (SELECT x.id FROM face x WHERE x.cluster = g.cluster AND +x.person IS NULL ORDER BY x.quality DESC, x.id LIMIT 6))
         FROM g ORDER BY g.n DESC, g.cluster",
    ) else {
        return Vec::new();
    };
    st.query_map(params![limit.min(i64::MAX as usize) as i64, offset as i64], |r| {
        let examples: Option<String> = r.get(2)?;
        Ok(Group { id: r.get(0)?, count: r.get::<_, i64>(1)? as usize, examples: examples.unwrap_or_default().split(',').filter_map(|x| x.parse().ok()).collect() })
    })
    .map(|r| r.flatten().collect())
    .unwrap_or_default()
}

/// Set all faces of a group to one name (existing or new person). Returns: count.
pub fn name_group(db: &crate::db::Db, group: i64, name: &str) -> Result<usize, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let person = find_or_create_person(db, name)?;
    let faces: Vec<(i64, i64)> = {
        let mut st = db.conn.prepare("SELECT id, image FROM face WHERE cluster=?1 AND person IS NULL AND status<>'rejected'").map_err(fe)?;
        let r = st.query_map([group], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(fe)?.collect::<rusqlite::Result<Vec<_>>>().map_err(fe)?;
        r
    };
    for (id, image) in &faces {
        db.conn.execute("UPDATE face SET person=?2, status='confirmed', local=1, cluster=NULL, suspect=0 WHERE id=?1", params![id, person]).map_err(fe)?;
        change_image_person(db, *image, *id, None, Some(person))?;
    }
    Ok(faces.len())
}

/// Do not name this face (a single face without a name): it no longer appears among the faces without a name.
pub fn reject_face(db: &crate::db::Db, face: i64) -> Result<(), String> {
    db.conn.execute("UPDATE face SET status='rejected', cluster=NULL, local=1 WHERE id=?1 AND person IS NULL", [face]).map(|_| ()).map_err(|e| e.to_string())
}

/// Do not name the group: its faces no longer appear in "Unbenannt".
pub fn discard_group(db: &crate::db::Db, group: i64) -> Result<(), String> {
    db.conn.execute("UPDATE face SET status='rejected', cluster=NULL, local=1 WHERE cluster=?1 AND person IS NULL", [group]).map(|_| ()).map_err(|e| e.to_string())
}

/// A face of a photo for the info panel.
#[derive(Clone, Debug)]
pub struct FaceInfo {
    pub id: i64,
    pub status: String,
    pub name: String,
    pub confidence: f32,
    pub x: f32,
    pub y: f32,
    pub b: f32,
    pub h: f32,
}

/// Faces of a photo, from left to right (not the rejected ones).
pub fn faces_to_image(c: &Connection, image: i64) -> Vec<FaceInfo> {
    let Ok(mut st) = c.prepare(
        "SELECT g.id, g.status, coalesce(s.name,''), coalesce(g.confidence,0), g.x, g.y, g.w, g.h FROM face g LEFT JOIN keyword s ON s.id=g.person
         WHERE g.image=?1 AND g.status<>'rejected' ORDER BY g.x",
    ) else {
        return Vec::new();
    };
    st.query_map([image], |r| Ok(FaceInfo { id: r.get(0)?, status: r.get(1)?, name: r.get(2)?, confidence: r.get::<_, f64>(3)? as f32, x: r.get::<_, f64>(4)? as f32, y: r.get::<_, f64>(5)? as f32, b: r.get::<_, f64>(6)? as f32, h: r.get::<_, f64>(7)? as f32 }))
        .map(|r| r.flatten().collect())
        .unwrap_or_default()
}

/// A person in the people list.
#[derive(Clone, Debug)]
pub struct PersonEntry {
    pub id: i64,
    pub name: String,
    pub count: usize,
    pub hidden: bool,
    pub preview: Option<i64>,
}

/// Confirmed faces of a person, newest shot first: (face, image, file name).
pub fn person_faces(c: &Connection, person: i64) -> Vec<(i64, i64, String)> {
    let Ok(mut st) = c.prepare(
        "SELECT g.id, g.image, coalesce(d.name,'') FROM face g JOIN image b ON b.id=g.image JOIN file d ON d.id=b.file
         WHERE g.person=?1 AND g.status='confirmed' ORDER BY d.capture_time DESC, g.id DESC",
    ) else {
        return Vec::new();
    };
    st.query_map([person], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// People with at least one confirmed face, by name (`hidden_items`: also the hidden ones).
pub fn person_list(c: &Connection, show_hidden: bool) -> Vec<PersonEntry> {
    let Ok(mut st) = c.prepare(
        "SELECT s.id, s.name, count(*), coalesce(pi.hidden,0),
                (SELECT g2.id FROM face g2 WHERE g2.person=s.id AND g2.status='confirmed' ORDER BY g2.embedding IS NOT NULL DESC, g2.quality DESC, g2.w DESC LIMIT 1)
         FROM face g JOIN keyword s ON s.id=g.person LEFT JOIN person_info pi ON pi.keyword=s.id
         WHERE g.status='confirmed' AND (?1 OR coalesce(pi.hidden,0)=0) GROUP BY s.id ORDER BY lower(s.name), s.id",
    ) else {
        return Vec::new();
    };
    st.query_map([show_hidden], |r| Ok(PersonEntry { id: r.get(0)?, name: r.get(1)?, count: r.get::<_, i64>(2)? as usize, hidden: r.get::<_, i64>(3)? != 0, preview: r.get(4)? }))
        .map(|r| r.flatten().collect())
        .unwrap_or_default()
}

/// Show or hide a person ("nicht anzeigen"): gets no suggestions and is not in the lists.
pub fn hide_person(c: &Connection, person: i64, hide: bool) -> Result<(), String> {
    c.execute("INSERT INTO person_info(keyword, hidden) VALUES(?1, ?2) ON CONFLICT(keyword) DO UPDATE SET hidden=excluded.hidden", params![person, hide]).map(|_| ()).map_err(|e| e.to_string())
}


/// Join faces into groups (indices into `faces`). A face goes into the group for which the **mean of the similarities
/// to all (up to 40 evenly chosen) members** is highest and at least `threshold` (average linkage);
/// two faces of the same photo never go into one group. High-quality faces first.
pub fn group_by(faces: &[&Probe], threshold: f32) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..faces.len()).collect();
    order.sort_by(|&a, &b| faces[b].quality.total_cmp(&faces[a].quality));
    let mut members: Vec<Vec<usize>> = Vec::new();
    let mut sums: Vec<Vec<f32>> = Vec::new();
    let mut centers: Vec<Vec<f32>> = Vec::new();
    let mut images: Vec<std::collections::HashSet<i64>> = Vec::new();
    for &i in &order {
        let p = faces[i];
        let mut best_match: Option<(usize, f32)> = None;
        for (g, m) in centers.iter().enumerate() {
            if images[g].contains(&p.image) || faces::sface::similarity(&p.embedding, m) < threshold - 0.12 {
                continue;
            }
            let ms = &members[g];
            let step = (ms.len() / 40).max(1);
            let (mut sum, mut n) = (0f32, 0f32);
            for &j in ms.iter().step_by(step) {
                sum += faces::sface::similarity(&p.embedding, &faces[j].embedding);
                n += 1.0;
            }
            let w = sum / n.max(1.0);
            if w >= threshold && best_match.is_none_or(|(_, b)| w > b) {
                best_match = Some((g, w));
            }
        }
        match best_match {
            Some((g, _)) => {
                for (a, b) in sums[g].iter_mut().zip(&p.embedding) {
                    *a += b;
                }
                let n = sums[g].iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
                centers[g] = sums[g].iter().map(|x| x / n).collect();
                members[g].push(i);
                images[g].insert(p.image);
            }
            None => {
                centers.push(p.embedding.clone());
                sums.push(p.embedding.clone());
                members.push(vec![i]);
                images.push(std::iter::once(p.image).collect());
            }
        }
    }
    members
}

/// Measurement: group confirmed faces (names hidden); how pure are the groups, how many faces end up in groups?
pub fn groups_test(c: &Connection) -> Result<String, String> {
    let all = load_samples(c)?;
    let faces: Vec<&Probe> = all.iter().filter(|p| p.status == "confirmed" && p.person.is_some() && p.quality >= 0.4).collect();
    let mut text = format!("{} bestätigte Gesichter (Qualität ≥ 0,4) ohne Namen gruppiert:\n", faces.len());
    for t in [0.45f32, 0.50, 0.55, 0.60, 0.65, 0.70] {
        let time = std::time::Instant::now();
        let gr = group_by(&faces, t);
        let (mut in_grp, mut pure, mut groups) = (0usize, 0usize, 0usize);
        let mut per_person: HashMap<i64, std::collections::HashSet<usize>> = HashMap::new();
        for (gi, g) in gr.iter().enumerate().filter(|(_, g)| g.len() >= 3) {
            groups += 1;
            in_grp += g.len();
            let mut z: HashMap<i64, usize> = HashMap::new();
            for &i in g {
                *z.entry(faces[i].person.unwrap()).or_default() += 1;
                per_person.entry(faces[i].person.unwrap()).or_default().insert(gi);
            }
            pure += z.values().max().copied().unwrap_or(0);
        }
        let split_count = per_person.values().filter(|s| s.len() > 1).count();
        text += &format!(
            "  Schwelle {t:.2}: {groups} Gruppen ab 3 Gesichtern, {:.0} % der Gesichter in Gruppen, Reinheit {:.1} %, {split_count} von {} Personen auf mehrere Gruppen verteilt ({:?})\n",
            100.0 * in_grp as f64 / faces.len() as f64,
            100.0 * pure as f64 / in_grp.max(1) as f64,
            per_person.len(),
            time.elapsed()
        );
    }
    Ok(text)
}

/// Assign an Apple group to a person after the user's confirmation (see [`crate::apple_groups`]).
pub fn apple_accept(db: &crate::db::Db, hint: &crate::apple_groups::Hint) -> Result<(), String> {
    crate::apple_groups::accept(db, hint).map(|_| ())
}
