//! Handover of the Lightroom develop data into the library – once, so that nothing reads the catalog
//! afterwards (Lightroom can then be removed and the catalog archived).
//!
//! For every photo of the library with develop settings in the catalog, the settings text of the
//! catalog (unchanged; for a white balance other than "As Shot" followed by Lightroom's own as-shot value as a
//! reference line, [`crate::lr_develop::AS_SHOT_KEY`]), the orientation in Lightroom and the "edited" mark go into the library
//! ([`crate::store`], `develop_state`); those of virtual copies into the copy's entry. Own develop settings of
//! graineo, ratings and keywords stay untouched. Since 08.11.2026 the library is the main store: the handover
//! takes seconds instead of hours; the sidecar next to the photo (same fields as before, [`crate::xmp::Lightroom`])
//! is written only if writing sidecars is on, in the background.
//!
//! Matching: first via the catalog file the library remembers (`file.lr_id`), otherwise via the path as
//! the app looked photos up in the catalog until now (name and the longest common tail of the path,
//! at least folder + name) – also for photos that moved (e.g. to the NAS with the same relative path).
//!
//! Repeatable and resumable: what is already in the library counts as "already there" and is not
//! written again. A photo taken over before the reference line existed gets it on the next run (only its Lightroom
//! data changes; own settings stay).
//! The catalog is only read ([`crate::lightroom::open_catalog`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::db::Db;
use crate::xmp::Lightroom;

/// An image (original or virtual copy) of the catalog with its develop data.
#[derive(Clone, Debug)]
pub struct CatalogImage {
    pub image: i64,
    pub master: Option<i64>,
    pub copy: Option<String>,
    /// settings text (`s = { … }`), only if not empty
    pub text: Option<String>,
    pub orientation: Option<String>,
    /// Lightroom marks the image as edited (`hasDevelopAdjustmentsEx`)
    pub edited: bool,
}

/// The develop data of a catalog, read once (read only).
pub struct Catalog {
    /// catalog file → path in the catalog
    paths: HashMap<i64, String>,
    /// (name, extension) in lower case → catalog files
    by_name: HashMap<(String, String), Vec<i64>>,
    /// catalog file → images (by image id)
    images: HashMap<i64, Vec<CatalogImage>>,
}

fn parts(p: &str) -> Vec<String> {
    p.split('/').filter(|t| !t.is_empty()).map(|t| crate::nfc(t).to_lowercase()).collect()
}

fn number(v: rusqlite::types::Value) -> f64 {
    match v {
        rusqlite::types::Value::Integer(i) => i as f64,
        rusqlite::types::Value::Real(r) => r,
        rusqlite::types::Value::Text(t) => t.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

impl Catalog {
    /// Read the catalog (read only).
    pub fn load(catalog: &Path) -> Result<Catalog, String> {
        let e = |e: rusqlite::Error| e.to_string();
        let conn = crate::lightroom::open_catalog(catalog).map_err(|e| format!("Katalog nicht lesbar: {e}"))?;
        // columns another Lightroom version may lack read as NULL (see `CatalogSchema`)
        let schema = crate::lightroom::CatalogSchema::read(&conn).map_err(e)?;
        for (table, columns) in [("AgLibraryFile", &["id_local", "folder", "baseName"][..]), ("AgLibraryFolder", &["id_local", "rootFolder"]), ("AgLibraryRootFolder", &["id_local", "absolutePath"]), ("Adobe_images", &["id_local", "rootFile"])] {
            schema.require(table, columns).map_err(|m| format!("Katalog nicht lesbar: {m} fehlt"))?;
        }
        let mut paths = HashMap::new();
        let mut by_name: HashMap<(String, String), Vec<i64>> = HashMap::new();
        {
            let mut st = conn
                .prepare(
                    &{
                        let (from_root, ext) = (schema.col("AgLibraryFolder", "fo", "pathFromRoot"), schema.col("AgLibraryFile", "f", "extension"));
                        format!(
                            "SELECT f.id_local, r.absolutePath || coalesce({from_root}, '') || f.baseName || '.' || coalesce({ext}, ''), f.baseName, coalesce({ext}, '')
                             FROM AgLibraryFile f JOIN AgLibraryFolder fo ON fo.id_local = f.folder JOIN AgLibraryRootFolder r ON r.id_local = fo.rootFolder"
                        )
                    },
                )
                .map_err(e)?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))).map_err(e)?;
            for row in rows {
                let (id, path, base, ext) = row.map_err(e)?;
                paths.insert(id, path);
                by_name.entry((base.to_ascii_lowercase(), ext.to_ascii_lowercase())).or_default().push(id);
            }
        }
        for v in by_name.values_mut() {
            v.sort_unstable();
        }
        let mut images: HashMap<i64, Vec<CatalogImage>> = HashMap::new();
        {
            let i = |c: &str| schema.col("Adobe_images", "i", c);
            let sql = if schema.require("Adobe_imageDevelopSettings", &["image"]).is_ok() {
                let d = |c: &str| schema.col("Adobe_imageDevelopSettings", "d", c);
                format!(
                    "SELECT i.id_local, i.rootFile, {}, {}, {}, {}, {}
                     FROM Adobe_images i LEFT JOIN Adobe_imageDevelopSettings d ON d.image = i.id_local ORDER BY i.id_local, {}",
                    i("masterImage"),
                    i("copyName"),
                    i("orientation"),
                    d("text"),
                    d("hasDevelopAdjustmentsEx"),
                    d("id_local")
                )
            } else {
                format!("SELECT i.id_local, i.rootFile, {}, {}, {}, NULL, NULL FROM Adobe_images i ORDER BY i.id_local", i("masterImage"), i("copyName"), i("orientation"))
            };
            let mut st = conn.prepare(&sql).map_err(e)?;
            let rows = st
                .query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, Option<String>>(5)?, number(r.get(6)?)))
                })
                .map_err(e)?;
            for row in rows {
                let (image, file, master, copy, orientation, text, edited) = row.map_err(e)?;
                let Some(file) = file else { continue };
                // Lightroom sometimes keeps a second, empty settings row next to the real one: the one with text counts
                // (surrounding white space is dropped – the sidecar keeps the text trimmed, the meaning is the same)
                let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                let list = images.entry(file).or_default();
                match list.iter_mut().find(|x| x.image == image) {
                    Some(x) => {
                        if x.text.is_none() {
                            x.text = text;
                        }
                        x.edited |= edited > 0.0;
                    }
                    None => list.push(CatalogImage { image, master, copy: copy.filter(|c| !c.is_empty()), text, orientation: orientation.filter(|o| !o.is_empty()), edited: edited > 0.0 }),
                }
            }
        }
        // Lightroom's own as-shot white balance as the reference of a changed white balance (only photos whose
        // white balance is not "As Shot"; see `lr_develop::AS_SHOT_KEY`)
        for list in images.values_mut() {
            for x in list.iter_mut() {
                if let Some(t) = x.text.take() {
                    x.text = Some(crate::lr_develop::attach_as_shot(&conn, x.image, x.master, t));
                }
            }
        }
        Ok(Catalog { paths, by_name, images })
    }

    /// Catalog files that fit the photo `image` by path (as the app looked photos up so far): same name, longest
    /// common tail of the path, at least folder + name. Several only if they fit equally well.
    pub fn by_path(&self, image: &Path) -> Vec<i64> {
        let Some(name) = image.file_name().and_then(|n| n.to_str()) else { return Vec::new() };
        let (root, ext) = name.rsplit_once('.').unwrap_or((name, ""));
        let Some(candidates) = self.by_name.get(&(root.to_ascii_lowercase(), ext.to_ascii_lowercase())) else { return Vec::new() };
        let target = parts(&image.to_string_lossy());
        let fits = |id: &i64| -> usize {
            let t = parts(self.paths.get(id).map(String::as_str).unwrap_or(""));
            t.iter().rev().zip(target.iter().rev()).take_while(|(a, b)| a == b).count()
        };
        let best = candidates.iter().map(fits).max().unwrap_or(0);
        if best < 2 {
            return Vec::new();
        }
        candidates.iter().copied().filter(|id| fits(id) == best).collect()
    }

    pub fn path_of(&self, file: i64) -> Option<&str> {
        self.paths.get(&file).map(String::as_str)
    }

    pub fn images_of(&self, file: i64) -> &[CatalogImage] {
        self.images.get(&file).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The original's data of a catalog file (settings text, orientation, mark), as the app used it.
    pub fn original(&self, file: i64) -> Lightroom {
        let Some(m) = self.images_of(file).iter().find(|i| i.master.is_none() && i.text.is_some()) else { return Lightroom::default() };
        Lightroom { develop: m.text.clone(), orientation: m.orientation.clone(), edited: m.edited, file: None }
    }

    /// Number of images (originals and copies) with settings text.
    pub fn with_settings(&self) -> usize {
        self.images.values().flatten().filter(|i| i.text.is_some()).count()
    }
}

/// Counts and protocol of a run.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub catalog: String,
    /// photos of the library looked at (without X3F and videos)
    pub photos: usize,
    /// of these with develop data in the catalog
    pub with_data: usize,
    /// sidecars written (or to be written in a check run)
    pub written: usize,
    /// virtual copies with their own Lightroom settings among them
    pub copies: usize,
    /// photos (originals and virtual copies) Lightroom marks as edited whose settings are in the library after this run
    /// (written now or already before). Not the same definition as "bearbeitet" when checking
    /// ([`crate::lightroom::Preview::edited`], all photos of the catalog marked as edited): photos whose file is not in
    /// the library, or without settings text, are missing here
    pub edited: usize,
    /// parts of these photos' Lightroom settings graineo does not take over (kind → count, see [`run_with`]) and the
    /// number of photos (originals and copies) with at least one of them
    pub not_taken: std::collections::BTreeMap<String, usize>,
    pub not_taken_photos: usize,
    /// already in the sidecar
    pub already: usize,
    /// catalog images with settings that are not in the library, and photos of the library missing on disk
    pub not_found: usize,
    /// sidecar folder not reachable (drive not mounted) – taken on the next run
    pub unreachable: usize,
    /// two photos of the library would share one sidecar – left out
    pub conflicts: usize,
    pub errors: usize,
    /// matched via `lr_id` while the path points to another catalog file (counted, `lr_id` applies)
    pub lr_id_differs: usize,
    /// several catalog files fit the path equally well (the first applies)
    pub ambiguous: usize,
    pub cancelled: bool,
    pub duration_ms: u128,
    /// one line per noteworthy photo (written, unreachable, not found …)
    pub log: Vec<String>,
}

impl Report {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "catalog": self.catalog,
            "photos": self.photos,
            "with_data": self.with_data,
            "written": self.written,
            "copies": self.copies,
            "edited": self.edited,
            "not_taken": self.not_taken,
            "not_taken_photos": self.not_taken_photos,
            "already": self.already,
            "not_found": self.not_found,
            "unreachable": self.unreachable,
            "conflicts": self.conflicts,
            "errors": self.errors,
            "lr_id_differs": self.lr_id_differs,
            "ambiguous": self.ambiguous,
            "cancelled": self.cancelled,
            "duration_ms": self.duration_ms,
        })
    }
}

struct LibraryFile {
    id: i64,
    lr_id: Option<i64>,
    path: PathBuf,
    /// 0 present, 1 missing, 2 volume missing
    status: i64,
}

/// Run the handover. `write = false`: check run, only counts (nothing is written, the library may be opened
/// read-only). `progress(done, total)` is called now and then; `cancel` stops after the current photo.
pub fn run(db: &Db, catalog: &Path, write: bool, progress: &dyn Fn(usize, usize), cancel: &AtomicBool) -> Result<Report, String> {
    run_with(db, catalog, write, progress, cancel, &|_| std::collections::BTreeMap::new())
}

/// [`run`] that also counts what of the settings graineo does not take over: `not_taken` gets each settings text of
/// the photos whose settings are in the library after the run (the interpretation lives in the develop crate).
pub fn run_with(
    db: &Db,
    catalog: &Path,
    write: bool,
    progress: &dyn Fn(usize, usize),
    cancel: &AtomicBool,
    not_taken: &dyn Fn(&str) -> std::collections::BTreeMap<String, usize>,
) -> Result<Report, String> {
    let t0 = std::time::Instant::now();
    let e = |e: rusqlite::Error| e.to_string();
    let cat = Catalog::load(catalog)?;
    // never the settings of another catalog (its ids collide with the ones of the library)
    {
        let files: HashMap<i64, String> = cat.paths.iter().map(|(id, p)| (*id, p.rsplit('/').next().unwrap_or(p).to_string())).collect();
        let images: HashMap<i64, i64> = cat.images.iter().flat_map(|(file, list)| list.iter().map(move |i| (i.image, *file))).collect();
        crate::lightroom::check_same_catalog(&db.conn, &files, &images)?;
    }
    let mut r = Report { catalog: catalog.display().to_string(), ..Default::default() };
    let files: Vec<LibraryFile> = {
        let mut st = db
            .conn
            .prepare(
                "SELECT d.id, d.lr_id, d.format, s.path || o.path || d.name, d.status FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
                 WHERE d.format NOT IN ('X3F', 'VIDEO') ORDER BY s.path || o.path, d.name",
            )
            .map_err(e)?;
        let rows = st.query_map([], |r| Ok(LibraryFile { id: r.get(0)?, lr_id: r.get(1)?, path: PathBuf::from(r.get::<_, String>(3)?), status: r.get(4)? })).map_err(e)?;
        rows.collect::<rusqlite::Result<_>>().map_err(e)?
    };
    // library copies by catalog image (name of the copy in the library)
    let copy_names: HashMap<i64, String> = {
        let mut st = db.conn.prepare("SELECT lr_id, copy FROM image WHERE copy IS NOT NULL AND lr_id IS NOT NULL").map_err(e)?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(e)?;
        rows.collect::<rusqlite::Result<_>>().map_err(e)?
    };
    let mut used: HashSet<i64> = HashSet::new();
    // first: match every photo with the catalog
    let mut plan: Vec<(&LibraryFile, i64)> = Vec::new();
    for f in &files {
        r.photos += 1;
        let by_path = cat.by_path(&f.path);
        let by_id = f.lr_id.filter(|id| cat.paths.contains_key(id));
        let Some(cf) = by_id.or_else(|| by_path.first().copied()) else { continue };
        if by_id.is_none() && by_path.len() > 1 {
            r.ambiguous += 1;
            r.log.push(format!("mehrdeutig (erster gilt): {}", f.path.display()));
        }
        if by_id.is_some() && !by_path.is_empty() && !by_path.contains(&cf) {
            r.lr_id_differs += 1;
            r.log.push(format!("lr_id ≠ Pfad: {} → {}", f.path.display(), cat.path_of(cf).unwrap_or("")));
        }
        used.insert(cf);
        if cat.original(cf).develop.is_none() && !cat.images_of(cf).iter().any(|i| i.master.is_some() && i.text.is_some()) {
            continue;
        }
        r.with_data += 1;
        plan.push((f, cf));
    }
    // then: into the library (the main store, `crate::store`) in short transactions; the sidecar next to the photo
    // follows in the background if writing sidecars is on. Photos on a location that is not mounted get their data
    // all the same – the library does not need the location.
    let total = plan.len();
    let mut tx = if write { Some(db.conn.unchecked_transaction().map_err(e)?) } else { None };
    for (n, (f, cf)) in plan.iter().enumerate() {
        let (f, cf) = (*f, *cf);
        if n % 200 == 0 {
            progress(n, total);
            if let Some(t) = tx.take() {
                t.commit().map_err(e)?;
                tx = Some(db.conn.unchecked_transaction().map_err(e)?);
            }
        }
        if cancel.load(Ordering::Relaxed) {
            r.cancelled = true;
            break;
        }
        // a photo that is gone (deleted or moved elsewhere) gets nothing
        if f.status == 1 {
            r.not_found += 1;
            r.log.push(format!("Foto fehlt: {}", f.path.display()));
            continue;
        }
        let mut lr = cat.original(cf);
        lr.file = Some(f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let present_all = crate::store::all_of_file(&db.conn, f.id).map_err(e)?;
        let present = present_all.get("").map(|x| x.lightroom.clone()).unwrap_or_default();
        let lr_new = if lr.develop.is_some() { lr } else { present.clone() };
        let copies: Vec<&CatalogImage> = cat.images_of(cf).iter().filter(|i| i.master.is_some() && i.text.is_some()).collect();
        let mut copy_changes: Vec<(String, Lightroom)> = Vec::new();
        for c in &copies {
            let name = copy_names.get(&c.image).cloned().or_else(|| c.copy.clone()).unwrap_or_else(|| "Kopie".into());
            let want = Lightroom { develop: c.text.clone(), orientation: c.orientation.clone(), ..Default::default() };
            if present_all.get(&name).map(|x| &x.lightroom) != Some(&want) && !copy_changes.iter().any(|(k, _)| *k == name) {
                copy_changes.push((name, want));
            }
        }
        // Lightroom's "edited" mark of the original (if its settings come along) and of the copies
        let edited = cat.images_of(cf).iter().filter(|i| i.edited && i.text.is_some()).count();
        for text in cat.images_of(cf).iter().filter_map(|i| i.text.as_deref()) {
            let parts = not_taken(text);
            if !parts.is_empty() {
                r.not_taken_photos += 1;
                for (k, n) in parts {
                    *r.not_taken.entry(k).or_default() += n;
                }
            }
        }
        if lr_new == present && copy_changes.is_empty() {
            r.already += 1;
            r.edited += edited;
            continue;
        }
        r.edited += edited;
        r.written += 1;
        r.copies += copies.len();
        if write {
            let store = || -> Result<(), String> {
                if lr_new != present {
                    crate::store::put_lightroom(&db.conn, f.id, None, &lr_new).map_err(e)?;
                }
                for (name, want) in &copy_changes {
                    // a copy of the catalog the library does not know yet becomes one (as the sync did with the sidecar's copies)
                    db.conn
                        .execute("INSERT INTO image(file, copy) SELECT ?1, ?2 WHERE NOT EXISTS (SELECT 1 FROM image WHERE file = ?1 AND copy = ?2)", rusqlite::params![f.id, name])
                        .map_err(e)?;
                    crate::store::put_lightroom(&db.conn, f.id, Some(name), want).map_err(e)?;
                }
                crate::store::queue_sidecar(&db.conn, f.id)
            };
            match store() {
                Ok(()) => r.log.push(format!("übernommen: {}", f.path.display())),
                Err(x) => {
                    r.written -= 1;
                    r.edited -= edited;
                    r.copies -= copies.len();
                    r.errors += 1;
                    r.log.push(format!("Fehler: {}: {x}", f.path.display()));
                }
            }
        }
    }
    if let Some(t) = tx.take() {
        t.commit().map_err(e)?;
    }
    progress(total, total);
    if !r.cancelled {
        let mut not_found: Vec<&str> = Vec::new();
        for (file, list) in &cat.images {
            if used.contains(file) {
                continue;
            }
            let n = list.iter().filter(|i| i.text.is_some()).count();
            if n > 0 {
                r.not_found += n;
                not_found.push(cat.path_of(*file).unwrap_or("?"));
            }
        }
        not_found.sort_unstable();
        r.log.extend(not_found.into_iter().map(|p| format!("nicht in der Bibliothek: {p}")));
    }
    r.duration_ms = t0.elapsed().as_millis();
    Ok(r)
}

/// Write the protocol of a run to the reports folder (`lr-handover-<time>.txt`). Returns its path.
pub fn save_protocol(r: &Report) -> Option<PathBuf> {
    let dir = crate::data_dir().join("reports");
    std::fs::create_dir_all(&dir).ok()?;
    let p = dir.join(format!("lr-handover-{}.txt", crate::now()));
    let mut t = serde_json::to_string_pretty(&r.json()).unwrap_or_default();
    t.push('\n');
    for l in &r.log {
        t.push_str(l);
        t.push('\n');
    }
    crate::files::write_atomic(&p, t.as_bytes()).ok()?;
    Some(p)
}
