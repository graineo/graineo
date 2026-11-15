//! Source management: all profile folders stay strictly read-only. Only references and
//! small header data for unreachable sources are stored.
use super::{FilmProfile, Look, read_profile};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};

pub const MAX_FILE: u64 = 16 * 1024 * 1024;
pub const MAX_FILES: usize = 5000;
pub const NOTICE: &str = "graineo enthält keine Filmprofile von Drittanbietern. Es liest Profile, die Sie selbst gekauft und installiert haben. Bitte beachten Sie die Lizenzbedingungen des Anbieters.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Automatic,
    Referenced,
}

#[derive(Clone, Debug)]
pub struct Storage {
    pub data: PathBuf,
    pub cache: PathBuf,
    pub automatic: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    #[serde(rename = "discoverFilmProfiles")]
    pub discover: bool,
    #[serde(rename = "filmProfileFolders")]
    pub folders: Vec<PathBuf>,
    #[serde(rename = "hiddenFilmPackages")]
    pub hidden_packages: HashSet<String>,
    #[serde(rename = "hiddenFilmProfiles")]
    pub hidden_profiles: HashSet<String>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            discover: true,
            folders: Vec::new(),
            hidden_packages: HashSet::new(),
            hidden_profiles: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub count: usize,
    pub referenced: usize,
    pub available: usize,
    pub automatic: usize,
    pub visible: bool,
    pub folders: Vec<PathBuf>,
}
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub imported: usize,
    pub existing: usize,
    pub skipped: usize,
    pub messages: Vec<String>,
}
impl Report {
    fn error(&mut self, p: &Path, text: impl std::fmt::Display) {
        self.skipped += 1;
        if self.messages.len() < 100 {
            self.messages.push(format!("{}: {text}", p.display()));
        }
    }
}

fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("GRAINEO_DATA_DIR") {
        return p.into();
    }
    sysdirs::app_data_dir().unwrap_or_default().join(sysdirs::APP)
}
impl Storage {
    pub fn standard() -> Self {
        let data = data_dir();
        let cache = if std::env::var_os("GRAINEO_DATA_DIR").is_some() {
            data.join("cache/film-looks")
        } else {
            sysdirs::app_cache_dir(sysdirs::APP).unwrap_or_default().join("film-looks")
        };
        let automatic = if let Some(p) = std::env::var_os("GRAINEO_FILM_PROFILE_DIRS") {
            std::env::split_paths(&p).collect()
        } else if std::env::var_os("GRAINEO_DATA_DIR").is_some()
            && std::env::var("GRAINEO_FILM_ADOBE").as_deref() != Ok("1")
        {
            Vec::new()
        } else {
            super::folders()
        };
        Self {
            data,
            cache,
            automatic,
        }
    }
    pub fn options(&self) -> Options {
        self.read_options().unwrap_or_default()
    }
    fn read_options(&self) -> Result<Options, String> {
        match fs::read(self.data.join("settings.json")) {
            Ok(b) => {
                serde_json::from_slice(&b).map_err(|e| format!("Einstellungen nicht lesbar: {e}"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Options::default()),
            Err(e) => Err(e.to_string()),
        }
    }
    pub fn write_options(&self, o: &Options) -> Result<(), String> {
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap();
        safe_folder(&self.data)?;
        let p = self.data.join("settings.json");
        if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("Einstellungsdatei ist eine Verknüpfung".into());
        }
        let mut v = match fs::read(&p) {
            Ok(b) => serde_json::from_slice::<serde_json::Value>(&b).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(e) => return Err(e.to_string()),
        };
        if !v.is_object() {
            return Err("Einstellungsdatei ist kein Objekt".into());
        }
        // Do not overwrite even valid JSON with broken known fields.
        serde_json::from_value::<Options>(v.clone()).map_err(|e| e.to_string())?;
        let values = serde_json::to_value(o).map_err(|e| e.to_string())?;
        for (k, w) in values.as_object().unwrap() {
            v[k] = w.clone();
        }
        write_atomic(
            &p,
            &serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?,
        )
    }
    pub fn show_package(&self, package: &str, show: bool) -> Result<(), String> {
        let mut o = self.options();
        if show {
            o.hidden_packages.remove(package);
        } else {
            o.hidden_packages.insert(package.into());
        }
        self.write_options(&o)?;
        reread();
        if !show {
            self.clear_old_cache()?;
        }
        Ok(())
    }
    pub fn show_profile(&self, id: &str, show: bool) -> Result<(), String> {
        let mut o = self.options();
        let id = id.to_uppercase();
        if show {
            o.hidden_profiles.remove(&id);
        } else {
            o.hidden_profiles.insert(id);
        }
        self.write_options(&o)?;
        reread();
        if !show {
            self.clear_old_cache()?;
        }
        Ok(())
    }
    /// Folder references only; removing never modifies a source file either.
    pub fn add_folder(&self, paths: &[PathBuf]) -> Report {
        let mut o = self.options();
        let mut b = Report::default();
        for p in paths {
            let result = (|| -> Result<bool, String> {
                if !p.is_absolute() || !p.is_dir() {
                    return Err("Bitte einen erreichbaren Profilordner wählen".into());
                }
                let p = p.canonicalize().map_err(|e| e.to_string())?;
                if o.folders.contains(&p) {
                    return Ok(false);
                }
                if o.folders.len() >= 64 {
                    return Err("Höchstens 64 Profilordner".into());
                }
                o.folders.push(p);
                Ok(true)
            })();
            match result {
                Ok(true) => b.imported += 1,
                Ok(false) => b.existing += 1,
                Err(e) => b.error(p, e),
            }
        }
        if b.imported > 0 {
            if let Err(e) = self.write_options(&o) {
                b.messages.push(e);
                b.skipped += b.imported;
                b.imported = 0;
            }
            reread();
        }
        b
    }
    pub fn remove_folder(&self, p: &Path) -> Result<(), String> {
        let mut o = self.options();
        o.folders.retain(|d| d != p);
        self.write_options(&o)?;
        reread();
        self.clear_old_cache()?;
        Ok(())
    }
    fn sources(&self, o: &Options) -> Vec<(PathBuf, Source)> {
        // Explicit sources first, so that the user's reference wins even at the search limit.
        let mut v: Vec<_> = o
            .folders
            .iter()
            .take(64)
            .filter(|p| p.is_absolute())
            .cloned()
            .map(|p| (p, Source::Referenced))
            .collect();
        if o.discover {
            v.extend(
                self.automatic
                    .iter()
                    .cloned()
                    .map(|p| (p, Source::Automatic)),
            );
        }
        v
    }
    pub fn search(&self) -> Vec<FilmProfile> {
        self.search_with_report().0
    }
    pub fn search_with_report(&self) -> (Vec<FilmProfile>, Report) {
        static INDEX_LOCK: Mutex<()> = Mutex::new(());
        let _lock = INDEX_LOCK.lock().unwrap();
        let o = match self.read_options() {
            Ok(o) => o,
            Err(e) => {
                return (
                    Vec::new(),
                    Report {
                        messages: vec![e],
                        ..Default::default()
                    },
                );
            }
        };
        let index_path = self.data.join("film-profile-index.json");
        let old = read_limited(&index_path, 32 * 1024 * 1024)
            .ok()
            .and_then(|b| serde_json::from_slice::<HeaderIndex>(&b).ok())
            .filter(|v| v.version == 1 && v.profiles.len() <= MAX_FILES)
            .unwrap_or_default();
        let mut profiles = Vec::new();
        let mut headers = Vec::new();
        let mut report = Report::default();
        let mut visited = 0;
        let mut roots = HashSet::new();
        for (basis, source) in self.sources(&o) {
            // An explicitly added source may replace the same automatic source.
            // Precedence when merging by UUID is deterministic below.
            if !roots.insert(basis.clone()) {
                continue;
            }
            if fs::read_dir(&basis).is_err() {
                report
                    .messages
                    .push(format!("Ordner nicht verfügbar: {}", basis.display()));
                for k in old
                    .profiles
                    .iter()
                    .filter(|k| k.source_folder == basis && k.path.starts_with(&basis))
                    .take(MAX_FILES.saturating_sub(headers.len()))
                {
                    profiles.push(k.profile(source, false));
                    headers.push(k.clone());
                }
                continue;
            }
            let (files, b) = files_found(&[basis.clone()], &mut visited);
            report.skipped += b.skipped;
            report.messages.extend(
                b.messages
                    .into_iter()
                    .take(100usize.saturating_sub(report.messages.len())),
            );
            for path in files {
                if headers.len() >= MAX_FILES {
                    break;
                }
                if let Some(mut f) = read_profile(&path) {
                    f.source = source;
                    f.name = f.name.chars().take(512).collect();
                    f.group = f.group.chars().take(512).collect();
                    headers.push(Header::from_profile(&f, &basis));
                    profiles.push(f);
                } else {
                    report.error(&path, "Kein lesbares unterstütztes Look-Profil");
                }
            }
        }
        // Available sources first; explicitly referenced sources win for the same UUID.
        let timings: HashMap<_, _> = profiles
            .iter()
            .filter(|p| p.available)
            .map(|p| {
                (
                    p.path.clone(),
                    fs::metadata(&p.path).and_then(|m| m.modified()).ok(),
                )
            })
            .collect();
        profiles.sort_by(|a, b| {
            (
                a.available,
                a.source == Source::Referenced,
                timings.get(&a.path),
                &a.path,
            )
                .cmp(&(
                    b.available,
                    b.source == Source::Referenced,
                    timings.get(&b.path),
                    &b.path,
                ))
        });
        let mut ids = BTreeMap::new();
        for p in profiles {
            ids.insert(p.uuid.clone(), p);
        }
        let mut profiles: Vec<_> = ids.into_values().collect();
        profiles.sort_by(|a, b| (&a.group, &a.name).cmp(&(&b.group, &b.name)));
        let index = HeaderIndex {
            version: 1,
            profiles: headers,
        };
        if let Ok(bytes) = serde_json::to_vec(&index) {
            if bytes.len() > 32 * 1024 * 1024 {
                report.messages.push(
                    "Profil-Kopfdaten überschreiten 32 MiB; Offline-Index bleibt unverändert"
                        .into(),
                );
                return (profiles, report);
            }
            // Header data contains neither the profile file nor the RGB table. An unknown version is left untouched.
            if !index_path.exists()
                || read_limited(&index_path, 32 * 1024 * 1024).is_ok_and(|b| {
                    b != bytes
                        && serde_json::from_slice::<HeaderIndex>(&b).is_ok_and(|i| i.version == 1)
                })
            {
                if let Err(e) = write_atomic(&index_path, &bytes) {
                    report
                        .messages
                        .push(format!("Profilindex nicht gespeichert: {e}"));
                }
            }
        }
        (profiles, report)
    }
    pub fn packages(&self) -> Vec<Package> {
        let o = self.options();
        let mut m: BTreeMap<String, Package> = BTreeMap::new();
        for p in self.search() {
            let t = m.entry(p.group.clone()).or_insert_with(|| Package {
                name: p.group.clone(),
                count: 0,
                referenced: 0,
                automatic: 0,
                available: 0,
                visible: !o.hidden_packages.contains(&p.group),
                folders: Vec::new(),
            });
            t.count += 1;
            if p.source == Source::Referenced {
                t.referenced += 1;
            } else {
                t.automatic += 1;
            }
            if p.available {
                t.available += 1;
            }
            if let Some(d) = p.path.parent() {
                if !t.folders.contains(&d.to_path_buf()) {
                    t.folders.push(d.into());
                }
            }
        }
        m.into_values().collect()
    }
    /// Only historical, uniquely named own table caches. No profile or source file.
    fn clear_old_cache(&self) -> Result<(), String> {
        if !self.cache.exists() {
            return Ok(());
        }
        safe_folder(&self.cache)?;
        let root = self.cache.canonicalize().map_err(|e| e.to_string())?;
        if adobe(&root)
            || fs::symlink_metadata(&self.cache).is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("Unsicherer alter Profilcache bleibt unangetastet".into());
        }
        for entry in fs::read_dir(&root).map_err(|e| e.to_string())?.take(10000) {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let safe = name
                .strip_prefix("look-")
                .and_then(|s| s.strip_suffix(".json"))
                .is_some_and(|s| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit()));
            if safe && is_own_file(&entry.path(), &root) {
                fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
}
fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
pub(crate) fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}
fn adobe(p: &Path) -> bool {
    p.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("Adobe") || s.eq_ignore_ascii_case("CameraRaw"))
    })
}
fn resolve(p: &Path) -> Result<PathBuf, String> {
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("Übergeordnete Pfadbestandteile sind nicht erlaubt".into());
    }
    if p.exists() {
        return p.canonicalize().map_err(|e| e.to_string());
    }
    let parent = p.parent().ok_or("Ungültiger Ordner")?;
    let name = p.file_name().ok_or("Ordnername fehlt")?;
    Ok(resolve(parent)?.join(name))
}
fn safe_folder(p: &Path) -> Result<(), String> {
    if fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Zielordner ist eine Verknüpfung".into());
    }
    let canon = resolve(p)?;
    if adobe(&canon) {
        return Err("In Adobe-Ordner wird niemals geschrieben".into());
    }
    // Existing intermediate symlinks must not redirect a managed write.
    let mut a = p.to_path_buf();
    while let Some(parent) = a.parent() {
        if fs::symlink_metadata(&a).is_ok_and(|m| m.file_type().is_symlink()) {
            // System aliases /var and /tmp are resolved above; user redirects inside application data are rejected.
            if a.components().count() > 2 {
                return Err("Zielpfad enthält eine Verknüpfung".into());
            }
        }
        a = parent.to_path_buf();
    }
    fs::create_dir_all(p).map_err(|e| e.to_string())
}
fn is_own_file(p: &Path, root: &Path) -> bool {
    if !fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()) {
        return false;
    }
    let (Ok(p), Ok(r)) = (p.canonicalize(), root.canonicalize()) else {
        return false;
    };
    p.starts_with(&r) && !adobe(&p)
}
pub(crate) fn read_limited(p: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max {
        return Err("Datei ist zu groß oder keine reguläre Datei".into());
    }
    let mut v = Vec::new();
    fs::File::open(p)
        .map_err(|e| e.to_string())?
        .take(max + 1)
        .read_to_end(&mut v)
        .map_err(|e| e.to_string())?;
    if v.len() as u64 > max {
        return Err("Datei ist zu groß".into());
    }
    Ok(v)
}
fn files_found(paths: &[PathBuf], visited: &mut usize) -> (Vec<PathBuf>, Report) {
    let mut out = Vec::new();
    let mut report = Report::default();
    let mut stack: Vec<_> = paths.iter().cloned().map(|p| (p, 0)).collect();
    while let Some((p, depth)) = stack.pop() {
        *visited += 1;
        if *visited > MAX_FILES * 4 || out.len() >= MAX_FILES {
            report.error(&p, "Suchgrenze erreicht (höchstens 5 000 Dateien)");
            break;
        }
        let Ok(meta) = fs::symlink_metadata(&p) else {
            report.error(&p, "Nicht lesbar");
            continue;
        };
        if meta.file_type().is_symlink() {
            report.error(&p, "Verknüpfung wird nicht verfolgt");
            continue;
        }
        if meta.is_dir() {
            if super::adobe_origin(&p) {
                report.error(&p, "Adobe-eigene Profile sind ausgeschlossen");
                continue;
            }
            if depth >= 12 {
                report.error(&p, "Zu viele Unterordner");
                continue;
            }
            match fs::read_dir(&p) {
                Ok(rd) => {
                    let mut children: Vec<_> = rd
                        .filter_map(Result::ok)
                        .map(|e| e.path())
                        .take(MAX_FILES * 4 + 1)
                        .collect();
                    children.sort();
                    stack.extend(children.into_iter().rev().map(|p| (p, depth + 1)));
                }
                Err(e) => report.error(&p, e),
            }
        } else if meta.is_file()
            && p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("xmp") || e.eq_ignore_ascii_case("cube"))
        {
            out.push(p)
        } else {
            report.error(&p, "Kein unterstütztes Profilformat")
        }
    }
    (out, report)
}
fn write_atomic(p: &Path, b: &[u8]) -> Result<(), String> {
    let parent = p.parent().ok_or("Ordner fehlt")?;
    safe_folder(parent)?;
    let tmp = parent.join(format!(
        "settings-film-{}-{}.tmp",
        std::process::id(),
        now_millis()
    ));
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    out.write_all(b)
        .and_then(|_| out.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp, p).map_err(|e| e.to_string())
}

static GENERATION: AtomicU64 = AtomicU64::new(1);
static CATALOGUE: OnceLock<Mutex<Option<Arc<Vec<FilmProfile>>>>> = OnceLock::new();
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}
pub fn reread() {
    if let Some(looks) = LOOKS.get() {
        looks.lock().unwrap().clear();
    }
    FAILED_LOOKS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap()
        .clear();
    *CATALOGUE.get_or_init(|| Mutex::new(None)).lock().unwrap() = None;
    GENERATION.fetch_add(1, Ordering::AcqRel);
}
pub fn installed() -> Arc<Vec<FilmProfile>> {
    let catalogue = CATALOGUE.get_or_init(|| Mutex::new(None));
    if let Some(v) = catalogue.lock().unwrap().as_ref() {
        return v.clone();
    }
    let before = generation();
    let v = Arc::new(Storage::standard().search());
    let mut g = catalogue.lock().unwrap();
    if generation() == before {
        *g = Some(v.clone());
    }
    v
}
pub fn visible_profiles() -> Vec<FilmProfile> {
    let o = Storage::standard().options();
    installed()
        .iter()
        .filter(|p| {
            !o.hidden_packages.contains(&p.group) && !o.hidden_profiles.contains(&p.uuid)
        })
        .cloned()
        .collect()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    id: String,
    name: String,
    package: String,
    path: PathBuf,
    source_folder: PathBuf,
    supports_amount: bool,
}
impl Header {
    fn from_profile(p: &FilmProfile, source: &Path) -> Self {
        Self {
            id: p.uuid.clone(),
            name: p.name.clone(),
            package: p.group.clone(),
            path: p.path.clone(),
            source_folder: source.into(),
            supports_amount: p.strength,
        }
    }
    fn profile(&self, source: Source, available: bool) -> FilmProfile {
        FilmProfile {
            uuid: self.id.clone(),
            name: self.name.clone(),
            group: self.package.clone(),
            path: self.path.clone(),
            source,
            available,
            strength: self.supports_amount,
            additional: Vec::new(),
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
struct HeaderIndex {
    version: u32,
    profiles: Vec<Header>,
}
static LOOKS: OnceLock<Mutex<HashMap<String, Arc<Look>>>> = OnceLock::new();
static FAILED_LOOKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
pub fn load_failed(id: &str) -> bool {
    FAILED_LOOKS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap()
        .contains(&id.to_uppercase())
}
pub(crate) fn look(f: &FilmProfile) -> Option<Look> {
    let result = look_intern(f);
    let mut failures = FAILED_LOOKS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap();
    if result.is_some() {
        failures.remove(&f.uuid);
    } else {
        failures.insert(f.uuid.clone());
    }
    result
}
fn look_intern(f: &FilmProfile) -> Option<Look> {
    if !f.available {
        return None;
    }
    let bytes = read_limited(&f.path, MAX_FILE).ok()?;
    let space = super::space_from_env();
    let digest = super::md5_hex(&super::md5(&bytes));
    let key = format!(
        "{:016x}",
        hash(format!("v1|{:?}|{digest}|{space:?}", f.path).as_bytes())
    );
    if let Some(l) = LOOKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(&key)
    {
        return Some((**l).clone());
    }
    // No disk cache: neither a profile file nor a derived table is stored.
    let text = std::str::from_utf8(&bytes).ok()?;
    let l = super::look_from_text(text, f).ok()?;
    let mut memory = LOOKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    if memory.len() >= 64 {
        memory.clear();
    }
    memory.insert(key, Arc::new(l.clone()));
    Some(l)
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
