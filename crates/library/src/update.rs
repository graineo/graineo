//! Update check and installation.
//!
//! Source: `latest.json` of the latest GitHub release (overridable for tests with
//! `GRAINEO_UPDATE_URL`) plus `latest.json.sig`, an Ed25519 signature over the exact bytes of
//! `latest.json` (base64). The app trusts the file only with a valid signature from one of the
//! built-in public keys ([`PUBLIC_KEY`], prepared for a key change: [`NEXT_PUBLIC_KEY`]), and a
//! downloaded package only if its size and SHA-256 match the signed file. Key handling
//! is described in the maintainer notes (key kept offline, second key prepared for a change).
//!
//! Privacy: the check is a plain GET of these two files with the user agent `graineo/<version>`;
//! nothing else is sent. Automatic checks run at most once in 24 hours, can be switched off
//! (setting `updateCheck`) and never happen in test runs (`GRAINEO_DATA_DIR` or a debug build)
//! unless `GRAINEO_UPDATE_URL` points to a test server. Test builds (version with `-`) never check.
//!
//! Threats and answers: a manipulated list or package is refused (signature, SHA-256, size, bundle
//! id, version, code signature; with a Developer ID also the team, [`DEVELOPER_TEAM_ID`]); an older
//! signed list never installs anything (downgrade: only strictly newer versions, checked again on the
//! unpacked app); the list is read with a size limit; https only (http only to 127.0.0.1 for tests);
//! downloads end on stalls and can be cancelled, a broken download leaves only its `.part` file,
//! which is removed; free space is checked before download and before unpacking; one installation at
//! a time (lock file, also across several running graineo processes); an app started from a disk
//! image or by macOS from a temporary read-only copy (App Translocation) is not updated in place –
//! the user is asked to move it to Applications first.
//!
//! Installation (macOS): unpack into a staging folder next to the running app, verify, then swap the
//! new app and the running one in a single atomic step (APFS `RENAME_SWAP`; on other file systems two
//! renames with restore on failure). The previous app stays in the staging folder until the new
//! version has run for a few seconds ([`confirm_start`]); only then it goes to the trash. If the new
//! version ends before confirming its start, the relaunch helper puts the previous app back and starts
//! it ([`relaunch`]); that version then reports the failed update and skips it. Windows: rename the
//! running `graineo.exe` to `graineo.old.exe`, put the new exe and `spektrafilm` in place, restart;
//! the leftovers are removed at the next start.
//!
//! Network access goes through the system's `curl` like the module downloads (no HTTP library).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde_json::{json, Value};

/// Address of the public repository – the one place it is written down (releases, update list, issues;
/// `scripts/release.sh` reads it from here for the download addresses in `latest.json`).
/// If the owner changes, change it only here and rebuild; shipped versions ask exactly this address.
#[macro_export]
macro_rules! repo_url {
    () => {
        "https://github.com/graineo/graineo"
    };
}
pub const REPO_URL: &str = crate::repo_url!();
/// New issue in the repository (Help → Report a Problem fills in title and text).
pub const NEW_ISSUE_URL: &str = concat!(crate::repo_url!(), "/issues/new");
/// Signed manifest of the latest release.
pub const DEFAULT_URL: &str = concat!(crate::repo_url!(), "/releases/latest/download/latest.json");
/// Opened when an update cannot be installed automatically.
pub const RELEASES_PAGE: &str = concat!(crate::repo_url!(), "/releases/latest");
/// Built-in Ed25519 public key (base64) of the release signing key. The private key never
/// leaves the release machine (`~/.graineo-signing/update-ed25519.key`, with offline copies).
pub const PUBLIC_KEY: &str = "lSBriFkBdKi6CodZo3LODp5MKS0+2hbhv+RjBj44Cl8=";
/// Second trusted key for a key change (lost or compromised key). The app accepts a signature from
/// either key, so releases can switch to the next key without stranding installed versions. The private key
/// (`~/.graineo-signing/update-ed25519-next.key`) is kept offline until the change.
pub const NEXT_PUBLIC_KEY: Option<&str> = Some("e9cC2SRGitqmODSHMKg6EZmC8PFz2hOaMwQaHDZr/jo=");
/// Team ID of the own Developer ID (FAHRPLAN, checklist 20). A downloaded app is installed only if its
/// code signature is a Developer ID of exactly this team (`codesign --verify -R`); since 1.0 the
/// packages are signed with it ("Developer ID Application", team LZSSGRN96K).
pub const DEVELOPER_TEAM_ID: Option<&str> = Some("LZSSGRN96K");
/// The signed list is small; anything larger is refused before it is parsed.
const MANIFEST_LIMIT: u64 = 1024 * 1024;
/// Free space kept in addition to what an update needs.
const SPACE_MARGIN: u64 = 200 * 1024 * 1024;
/// Seconds the new version must run before the previous one goes to the trash.
pub const CONFIRM_AFTER: f64 = 3.0;
/// Bundle identifier the new macOS app must carry.
pub const BUNDLE_ID: &str = include_str!("../../../scripts/bundle-id.txt").trim_ascii();
/// Minimum time between two automatic checks (seconds).
pub const CHECK_INTERVAL: i64 = 24 * 3600;

/// Settings keys (`settings.json`).
pub const KEY_ENABLED: &str = "updateCheck";
pub const KEY_LAST_CHECK: &str = "updateLastCheck";
pub const KEY_SKIPPED: &str = "updateSkippedVersion";

/// Asset of this platform in `latest.json`.
pub fn asset_key() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("macos-universal")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x64")
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub version: String,
    pub date: String,
    /// Release notes (Markdown, English)
    pub notes: String,
    /// Release notes in German (optional)
    pub notes_de: Option<String>,
    pub assets: Vec<(String, Asset)>,
}

impl Manifest {
    pub fn asset(&self, key: &str) -> Option<&Asset> {
        self.assets.iter().find(|(k, _)| k == key).map(|(_, a)| a)
    }
    /// Notes in the UI language (German if present, otherwise English).
    pub fn notes_for(&self, german: bool) -> &str {
        match &self.notes_de {
            Some(d) if german && !d.trim().is_empty() => d,
            _ => &self.notes,
        }
    }
}

// ─────────────────────────────────────────────── versions, schedule

/// `0.63.0` (optionally with a leading `v`) → (0, 63, 0). Pre-release suffixes are not accepted.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut it = s.split('.');
    let mut part = || -> Option<u64> {
        let p = it.next()?;
        if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let v = (part()?, part()?, part()?);
    if it.next().is_some() {
        return None;
    }
    Some(v)
}

/// Is `candidate` strictly newer than `current`? Unparsable versions are never newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Test builds (`0.86.2-leistung.1`) carry a `-` in the version: they never look for an update and never offer one,
/// so a test app next to the installed one cannot replace itself or be replaced.
pub fn is_prerelease(version: &str) -> bool {
    version.trim().contains('-')
}

/// 24-hour rule: due if never checked, if the last check is at least [`CHECK_INTERVAL`] ago, or if
/// the clock went backwards.
pub fn due(last: Option<i64>, now: i64) -> bool {
    match last {
        None => true,
        Some(l) => now < l || now - l >= CHECK_INTERVAL,
    }
}

// ─────────────────────────────────────────────── signature

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn key_from_b64(key_b64: &str) -> Result<VerifyingKey, String> {
    let bytes = b64().decode(key_b64.trim()).map_err(|_| "public key: invalid base64".to_string())?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "public key: wrong length".to_string())?;
    VerifyingKey::from_bytes(&arr).map_err(|_| "public key: invalid".to_string())
}

/// Verify an Ed25519 signature (base64) over `data` with the public key `key_b64` (base64).
pub fn verify_signature(data: &[u8], sig_b64: &str, key_b64: &str) -> Result<(), String> {
    let key = key_from_b64(key_b64)?;
    let sig = b64().decode(sig_b64.trim()).map_err(|_| "signature: invalid base64".to_string())?;
    let sig: [u8; 64] = sig.try_into().map_err(|_| "signature: wrong length".to_string())?;
    key.verify_strict(data, &Signature::from_bytes(&sig)).map_err(|_| "signature does not match".to_string())
}

/// The keys built into the app: the current one and, once generated, the next one.
pub fn built_in_keys() -> Vec<&'static str> {
    std::iter::once(PUBLIC_KEY).chain(NEXT_PUBLIC_KEY).collect()
}

/// Keys the app trusts. Debug builds accept a test key from `GRAINEO_UPDATE_TEST_PUBKEY` instead;
/// in a release build the built-in keys can never be replaced.
pub fn trusted_keys() -> Vec<String> {
    #[cfg(debug_assertions)]
    if let Ok(k) = std::env::var("GRAINEO_UPDATE_TEST_PUBKEY") {
        if !k.trim().is_empty() {
            return vec![k];
        }
    }
    built_in_keys().into_iter().map(str::to_string).collect()
}

/// Valid if the signature matches any of `keys`.
pub fn verify_any(data: &[u8], sig_b64: &str, keys: &[String]) -> Result<(), String> {
    let mut last = Err("no trusted key".to_string());
    for k in keys {
        match verify_signature(data, sig_b64, k) {
            Ok(()) => return Ok(()),
            Err(e) => last = Err(e),
        }
    }
    last
}

/// Signing side (release script via `graineo-dev`): secret seed (base64, 32 bytes) → signature (base64).
pub fn sign(data: &[u8], seed_b64: &str) -> Result<String, String> {
    let key = signing_key(seed_b64)?;
    Ok(b64().encode(key.sign(data).to_bytes()))
}

/// Public key (base64) belonging to a secret seed (base64).
pub fn public_key_of(seed_b64: &str) -> Result<String, String> {
    Ok(b64().encode(signing_key(seed_b64)?.verifying_key().to_bytes()))
}

/// New secret seed (base64) from the operating system's random source.
pub fn new_seed() -> Result<String, String> {
    use std::io::Read;
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut seed)).map_err(|e| format!("random source: {e}"))?;
    if seed.iter().all(|&b| b == 0) {
        return Err("random source returned zeros".into());
    }
    Ok(b64().encode(seed))
}

fn signing_key(seed_b64: &str) -> Result<SigningKey, String> {
    let bytes = b64().decode(seed_b64.trim()).map_err(|_| "secret key: invalid base64".to_string())?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "secret key: wrong length".to_string())?;
    Ok(SigningKey::from_bytes(&arr))
}

// ─────────────────────────────────────────────── manifest

/// Address of `latest.json` and whether it is a test override.
pub fn source_url() -> (String, bool) {
    match std::env::var("GRAINEO_UPDATE_URL") {
        Ok(u) if !u.trim().is_empty() => (u.trim().to_string(), true),
        _ => (DEFAULT_URL.to_string(), false),
    }
}

/// Downloads must be https; with a test server (`GRAINEO_UPDATE_URL`) also http://127.0.0.1.
pub fn url_allowed(url: &str, allow_local: bool) -> bool {
    let ok_https = url.starts_with("https://") && url.len() > 8;
    let ok_local = allow_local && (url.starts_with("http://127.0.0.1:") || url.starts_with("http://127.0.0.1/"));
    (ok_https || ok_local) && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Parse and validate `latest.json` (only call after the signature was verified).
pub fn parse_manifest(bytes: &[u8], allow_local: bool) -> Result<Manifest, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("latest.json: {e}"))?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let version = s("version").ok_or("latest.json: version missing")?;
    if parse_version(&version).is_none() {
        return Err(format!("latest.json: invalid version {version}"));
    }
    let mut assets = Vec::new();
    for (k, a) in v.get("assets").and_then(Value::as_object).ok_or("latest.json: assets missing")? {
        let url = a.get("url").and_then(Value::as_str).ok_or_else(|| format!("latest.json: {k}: url missing"))?;
        let sha = a.get("sha256").and_then(Value::as_str).ok_or_else(|| format!("latest.json: {k}: sha256 missing"))?.to_ascii_lowercase();
        let size = a.get("size").and_then(Value::as_u64).ok_or_else(|| format!("latest.json: {k}: size missing"))?;
        if !url_allowed(url, allow_local) {
            return Err(format!("latest.json: {k}: address not allowed"));
        }
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("latest.json: {k}: invalid sha256"));
        }
        if size == 0 {
            return Err(format!("latest.json: {k}: size 0"));
        }
        assets.push((k.clone(), Asset { url: url.to_string(), sha256: sha, size }));
    }
    Ok(Manifest { version, date: s("date").unwrap_or_default(), notes: s("notes").unwrap_or_default(), notes_de: s("notes_de").filter(|t| !t.trim().is_empty()), assets })
}

/// Write `latest.json` (release script).
pub fn manifest_json(m: &Manifest) -> String {
    let mut assets = serde_json::Map::new();
    for (k, a) in &m.assets {
        assets.insert(k.clone(), json!({"url": a.url, "sha256": a.sha256, "size": a.size}));
    }
    let mut o = json!({"version": m.version, "date": m.date, "notes": m.notes, "assets": assets});
    if let Some(d) = &m.notes_de {
        o["notes_de"] = json!(d);
    }
    serde_json::to_string_pretty(&o).unwrap_or_default() + "\n"
}

// ─────────────────────────────────────────────── settings

pub fn enabled() -> bool {
    crate::settings::value(KEY_ENABLED).as_bool().unwrap_or(true)
}

pub fn set_enabled(on: bool) {
    crate::settings::set(KEY_ENABLED, json!(on));
}

pub fn skipped_version() -> Option<String> {
    crate::settings::value(KEY_SKIPPED).as_str().map(str::to_string)
}

pub fn skip_version(v: &str) {
    crate::settings::set(KEY_SKIPPED, json!(v));
}

pub fn last_check() -> Option<i64> {
    crate::settings::value(KEY_LAST_CHECK).as_i64()
}

/// May the app check automatically now? Off in the settings → never. Test runs (own data folder,
/// debug build) only with a test server (`GRAINEO_UPDATE_URL`); then the 24-hour rule is skipped.
pub fn auto_check_due() -> bool {
    if is_prerelease(&current_version()) {
        return false;
    }
    if !enabled() || asset_key().is_none() {
        return false;
    }
    let (_, test) = source_url();
    if test {
        return true;
    }
    if std::env::var_os("GRAINEO_DATA_DIR").is_some() || std::env::var_os("GRAINEO_LIBRARY_DB").is_some() || cfg!(debug_assertions) {
        return false;
    }
    due(last_check(), crate::now())
}

// ─────────────────────────────────────────────── network

/// Command without a console window on Windows.
fn command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn user_agent() -> String {
    format!("graineo/{}", env!("CARGO_PKG_VERSION"))
}

fn curl_base(allow_local: bool) -> std::process::Command {
    let proto = if allow_local { "=http,https" } else { "=https" };
    let mut c = command("curl");
    c.args(["-sSfL", "--proto", proto, "--proto-redir", proto, "--max-redirs", "5", "--connect-timeout", "20", "-A"]).arg(user_agent());
    c
}

/// Small file (list, signature): at most [`MANIFEST_LIMIT`] bytes, 30 s.
fn get(url: &str, allow_local: bool) -> Result<Vec<u8>, String> {
    if !url_allowed(url, allow_local) {
        return Err(format!("address not allowed: {url}"));
    }
    let out = curl_base(allow_local).args(["--max-time", "30", "--max-filesize"]).arg(MANIFEST_LIMIT.to_string()).arg(url).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    if out.stdout.len() as u64 > MANIFEST_LIMIT {
        return Err(format!("{url}: larger than {MANIFEST_LIMIT} bytes"));
    }
    Ok(out.stdout)
}

/// Did the download fail because the address does not exist (HTTP 404, curl `-f`)?
fn not_found(error: &str) -> bool {
    error.contains("error: 404")
}

/// Result of a check.
#[derive(Clone, Debug)]
pub enum Check {
    UpToDate { current: String, latest: String },
    Available(Manifest),
}

/// Verify the downloaded manifest bytes and its signature and compare with `current`.
pub fn evaluate(json: &[u8], sig: &str, keys: &[String], current: &str, allow_local: bool) -> Result<Check, String> {
    if is_prerelease(current) {
        return Ok(Check::UpToDate { current: current.to_string(), latest: current.to_string() });
    }
    verify_any(json, sig, keys)?;
    let m = parse_manifest(json, allow_local)?;
    let key = asset_key().ok_or("no update package for this system")?;
    if is_newer(&m.version, current) {
        if m.asset(key).is_none() {
            return Err(format!("latest.json: no package {key}"));
        }
        Ok(Check::Available(m))
    } else {
        Ok(Check::UpToDate { current: current.to_string(), latest: m.version })
    }
}

/// Fetch `latest.json` and its signature, verify and compare. Records the time of the check.
pub fn check() -> Result<Check, String> {
    let current = current_version();
    if is_prerelease(&current) {
        return Ok(Check::UpToDate { latest: current.clone(), current });
    }
    let (url, test) = source_url();
    crate::settings::set(KEY_LAST_CHECK, json!(crate::now()));
    let allow_local = test;
    let json = match get(&url, allow_local) {
        // no release published yet (repository or release missing): nothing to offer, no error for the user
        Err(e) if not_found(&e) => return Ok(Check::UpToDate { latest: current.clone(), current }),
        r => r?,
    };
    let sig = get(&format!("{url}.sig"), allow_local)?;
    let sig = String::from_utf8(sig).map_err(|_| "signature: not text".to_string())?;
    evaluate(&json, &sig, &trusted_keys(), &current, allow_local)
}

/// Download folder for packages (cache; with `GRAINEO_DATA_DIR` inside the test data folder).
pub fn downloads_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("GRAINEO_DATA_DIR") {
        return PathBuf::from(p).join("updates");
    }
    sysdirs::app_cache_dir(crate::APP).unwrap_or_else(|| std::env::temp_dir().join(crate::APP)).join("updates")
}

/// Download a package with progress (`progress(bytes, total)`) and verify size and SHA-256.
/// Returns the verified file.
pub fn download(m: &Manifest, key: &str, progress: &dyn Fn(u64, u64), cancel: &AtomicBool) -> Result<PathBuf, String> {
    let a = m.asset(key).ok_or_else(|| format!("no package {key}"))?;
    let (_, allow_local) = source_url();
    if !url_allowed(&a.url, allow_local) {
        return Err("address not allowed".into());
    }
    let dir = downloads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = format!("graineo-{}-{key}.zip", m.version);
    let dest = dir.join(&name);
    let part = dir.join(format!("{name}.part"));
    let _ = std::fs::remove_file(&part);
    let mut child = curl_base(allow_local).args(["--speed-limit", "1024", "--speed-time", "60", "-o"]).arg(&part).arg(&a.url).spawn().map_err(|e| format!("curl: {e}"))?;
    loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            if !st.success() {
                let _ = std::fs::remove_file(&part);
                return Err("download failed".into());
            }
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&part);
            return Err("cancelled".into());
        }
        let n = std::fs::metadata(&part).map_or(0, |m| m.len());
        if n > a.size {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&part);
            return Err("package larger than announced".into());
        }
        progress(n, a.size);
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    verify_file(&part, a).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })?;
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    progress(a.size, a.size);
    Ok(dest)
}

/// Size and SHA-256 of a file as announced in the signed manifest.
pub fn verify_file(p: &Path, a: &Asset) -> Result<(), String> {
    let n = std::fs::metadata(p).map_err(|e| e.to_string())?.len();
    if n != a.size {
        return Err(format!("size {n} instead of {}", a.size));
    }
    if crate::relocate::sha256(p)? != a.sha256 {
        return Err("checksum does not match".into());
    }
    Ok(())
}

// ─────────────────────────────────────────────── installation

/// Why an installation did not happen.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallError {
    /// The app's folder is not writable (e.g. /Applications without rights): install by hand from
    /// the release page.
    NotWritable(String),
    /// The app runs from a disk image or from a temporary read-only copy that macOS made at the first
    /// start (App Translocation): move it to Applications first.
    NotMoved,
    /// Not enough free space (needed, free; bytes).
    NoSpace { need: u64, free: u64 },
    /// macOS refused to change the app (Privacy & Security → App Management).
    Blocked,
    /// Another graineo is installing an update right now.
    Busy,
    /// Not started from an installed app (e.g. development build): install by hand.
    NotInstalled,
    Failed(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWritable(p) => write!(f, "not writable: {p}"),
            Self::NotMoved => write!(f, "running from a disk image or a translocated copy"),
            Self::NoSpace { need, free } => write!(f, "not enough space: {need} bytes needed, {free} free"),
            Self::Blocked => write!(f, "macOS refused to change the app (App Management)"),
            Self::Busy => write!(f, "another installation is running"),
            Self::NotInstalled => write!(f, "not running from an installed app"),
            Self::Failed(e) => write!(f, "{e}"),
        }
    }
}

fn unique(prefix: &str) -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{prefix}{}{n:08x}", std::process::id())
}

/// Create a fresh hidden staging folder in `dir`; fails if `dir` is not writable.
fn staging_in(dir: &Path) -> Result<PathBuf, InstallError> {
    let s = dir.join(unique(".graineo-update-"));
    std::fs::create_dir(&s).map_err(|_| InstallError::NotWritable(dir.display().to_string()))?;
    Ok(s)
}

/// Check before downloading that the app's folder takes the new version (creates and removes a
/// staging folder).
pub fn writable(dir: &Path) -> Result<(), InstallError> {
    let s = staging_in(dir)?;
    let _ = std::fs::remove_dir(&s);
    Ok(())
}

/// Is the app somewhere it cannot be updated in place? Started from a disk image (read-only volume) or,
/// at the first start after a download, from the read-only copy macOS makes (App Translocation).
pub fn location_problem(bundle: &Path) -> Option<InstallError> {
    let s = bundle.to_string_lossy();
    (s.contains("/AppTranslocation/") || read_only_volume(bundle)).then_some(InstallError::NotMoved)
}

#[cfg(target_os = "macos")]
fn read_only_volume(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid C string and a zeroed out-buffer of the right type
    (unsafe { libc::statfs(c.as_ptr(), &mut st) } == 0) && st.f_flags & libc::MNT_RDONLY as u32 != 0
}

#[cfg(not(target_os = "macos"))]
fn read_only_volume(_: &Path) -> bool {
    false
}

/// Is there room for `need` bytes (plus a margin) in `dir`? Unknown free space counts as enough.
pub fn check_space(dir: &Path, need: u64) -> Result<(), InstallError> {
    let need = need.saturating_add(SPACE_MARGIN);
    match crate::platform::free_bytes(dir) {
        Some(free) if free < need => Err(InstallError::NoSpace { need, free }),
        _ => Ok(()),
    }
}

/// Space an update needs next to the app: the unpacked app is at most this many times the package.
pub const UNPACKED_FACTOR: u64 = 4;

/// State of the updater (lock, record of the last installation, start markers): kept with the app's
/// data (with `GRAINEO_DATA_DIR` inside the test data folder), not in the cache.
pub fn state_dir() -> PathBuf {
    crate::data_dir().join("updates")
}

/// Held while an update downloads and installs: a second graineo process (or a second click) gets
/// [`InstallError::Busy`]. Released when dropped or when the process ends.
pub struct Lock(#[allow(dead_code)] std::fs::File);

pub fn lock() -> Result<Lock, InstallError> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| InstallError::Failed(format!("{}: {e}", dir.display())))?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("install.lock")).map_err(|e| InstallError::Failed(e.to_string()))?;
    match f.try_lock() {
        Ok(()) => Ok(Lock(f)),
        Err(std::fs::TryLockError::WouldBlock) => Err(InstallError::Busy),
        Err(std::fs::TryLockError::Error(e)) => Err(InstallError::Failed(format!("lock: {e}"))),
    }
}

/// Current version: on macOS the running bundle's `CFBundleShortVersionString`, otherwise the build's.
/// A test version in the bundle (`0.107.0-test`) counts as such, so it never checks.
pub fn current_version() -> String {
    #[cfg(target_os = "macos")]
    if let Some(v) = running_bundle().and_then(|b| plist_value(&b.join("Contents/Info.plist"), "CFBundleShortVersionString")) {
        if parse_version(&v).is_some() || is_prerelease(&v) {
            return v;
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// The `.app` this process runs from (macOS).
pub fn running_bundle() -> Option<PathBuf> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension().is_some_and(|e| e == "app")).then(|| app.to_path_buf())
}

#[cfg(target_os = "macos")]
fn plist_value(plist: &Path, key: &str) -> Option<String> {
    let out = command("/usr/bin/plutil").args(["-extract", key, "raw", "-o", "-"]).arg(plist).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let out = command(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{program}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Move within or across volumes (like `mv`).
#[cfg(target_os = "macos")]
fn move_path(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    run("/bin/mv", &[from.as_os_str(), to.as_os_str()])
}

/// Exchange two paths in one atomic step (APFS). `Ok(false)`: the file system cannot swap.
#[cfg(target_os = "macos")]
fn swap(a: &Path, b: &Path) -> Result<bool, std::io::Error> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: two valid C strings
    if unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) } == 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::EXDEV) => Ok(false),
        _ => Err(e),
    }
}

/// macOS answers a change of a protected app with "Operation not permitted" (App Management).
fn blocked(e: &str) -> bool {
    e.contains("Operation not permitted") || e.contains("os error 1)")
}

/// Team ID the new app must be signed with: the built-in one, or the running app's own when it runs
/// with a Developer ID (an app signed by the team never accepts an update without that signature).
#[cfg(target_os = "macos")]
pub fn required_team(running: &Path) -> Option<String> {
    DEVELOPER_TEAM_ID.map(str::to_string).or_else(|| team_of(running))
}

/// Team ID of a signed app (`None`: ad-hoc or unsigned).
#[cfg(target_os = "macos")]
pub fn team_of(app: &Path) -> Option<String> {
    let out = command("/usr/bin/codesign").args(["-d", "--verbose=2"]).arg(app).output().ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let team = text.lines().find_map(|l| l.strip_prefix("TeamIdentifier="))?.trim().to_string();
    (team != "not set" && !team.is_empty()).then_some(team)
}

/// Verify the code signature of an app; with `team`, it must be a Developer ID of that team.
#[cfg(target_os = "macos")]
pub fn verify_code(app: &Path, team: Option<&str>) -> Result<(), String> {
    let mut args: Vec<std::ffi::OsString> = vec!["--verify".into(), "--deep".into(), "--strict".into()];
    if let Some(t) = team {
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("invalid team id".into());
        }
        // Developer ID Application certificate of exactly this team, issued by Apple
        args.push("--test-requirement".into());
        args.push(format!("=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"{t}\"").into());
    }
    args.push(app.as_os_str().into());
    let refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    run("/usr/bin/codesign", &refs).map_err(|e| format!("code signature: {e}"))
}

/// What happened during an installation (for the report, the log and the relaunch).
#[derive(Debug, Clone, Default)]
pub struct Installed {
    /// What to start after quitting
    pub relaunch: PathBuf,
    /// The previous app, kept until the new version confirmed its start (macOS)
    pub previous: Option<PathBuf>,
    /// Version that was installed
    pub version: String,
    /// Problems that did not stop the installation
    pub warnings: Vec<String>,
}

/// Record of an installation whose new version has not yet confirmed its start (`pending.json`).
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub version: String,
    pub from: String,
    pub bundle: PathBuf,
    pub previous: PathBuf,
    pub stage: PathBuf,
    pub time: i64,
}

fn pending_path() -> PathBuf {
    state_dir().join("pending.json")
}

/// Written by the new version once it has run for [`CONFIRM_AFTER`] seconds; the relaunch helper waits for it.
pub fn started_marker(version: &str) -> PathBuf {
    state_dir().join(format!("started-{version}"))
}

/// Written by the relaunch helper when it put the previous app back.
pub fn rolled_back_marker(version: &str) -> PathBuf {
    state_dir().join(format!("rolled-back-{version}"))
}

fn write_pending(p: &Pending) -> Result<(), String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let j = json!({"version": p.version, "from": p.from, "bundle": p.bundle, "previous": p.previous, "stage": p.stage, "time": p.time});
    let tmp = dir.join("pending.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&j).unwrap_or_default()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, pending_path()).map_err(|e| e.to_string())
}

pub fn read_pending() -> Option<Pending> {
    let v: Value = serde_json::from_slice(&std::fs::read(pending_path()).ok()?).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Pending {
        version: s("version")?,
        from: s("from")?,
        bundle: s("bundle")?.into(),
        previous: s("previous")?.into(),
        stage: s("stage")?.into(),
        time: v.get("time").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn clear_pending() {
    let _ = std::fs::remove_file(pending_path());
}

/// Remove an own staging folder once no app is left in it (never removes an app).
fn remove_stage(stage: &Path) {
    if !stage.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".graineo-update-")) {
        return;
    }
    fn has_app(p: &Path) -> bool {
        std::fs::read_dir(p).map(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x == "app") || (e.path().is_dir() && has_app(&e.path())))).unwrap_or(false)
    }
    if !has_app(stage) {
        let _ = std::fs::remove_dir_all(stage);
    }
}

/// Outcome of the previous installation, asked once at start.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// nothing pending (or nothing to report)
    Nothing,
    /// the new version is running; call [`confirm_start`] after [`CONFIRM_AFTER`] seconds
    Started(String),
    /// the new version did not start; the previous app (this one) was put back. The version is skipped.
    RolledBack(String),
}

/// At start: what became of the last installation? A failed one is cleaned up here (the copy that did
/// not start goes to `dispose`, i.e. the trash) and its version is skipped for automatic offers.
pub fn outcome(current: &str, dispose: &dyn Fn(&Path) -> Result<(), String>) -> Outcome {
    // markers of earlier, finished installations
    if let Ok(rd) = std::fs::read_dir(state_dir()) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("started-") && n != format!("started-{current}") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let Some(p) = read_pending() else { return Outcome::Nothing };
    let rb = rolled_back_marker(&p.version);
    if rb.exists() {
        let failed = p.stage.join("failed");
        if let Ok(rd) = std::fs::read_dir(&failed) {
            for e in rd.flatten() {
                if let Err(err) = dispose(&e.path()) {
                    eprintln!("update: failed copy not moved to the trash: {err}");
                }
            }
        }
        remove_stage(&p.stage);
        let _ = std::fs::remove_file(&rb);
        clear_pending();
        skip_version(&p.version);
        return Outcome::RolledBack(p.version);
    }
    if current == p.version {
        return Outcome::Started(p.version);
    }
    // the exchange did not happen (ended before it) or the previous version was put back by hand:
    // own unpacked files go, an app is never removed here
    if !p.previous.exists() || crate::now() - p.time > 7 * 86_400 {
        remove_stage(&p.stage);
        clear_pending();
    }
    Outcome::Nothing
}

/// The new version has run long enough: tell the relaunch helper, put the previous app in the trash
/// (`dispose`), tidy up. Returns the confirmed version.
pub fn confirm_start(current: &str, dispose: &dyn Fn(&Path) -> Result<(), String>) -> Option<String> {
    let p = read_pending().filter(|p| p.version == current)?;
    let _ = std::fs::create_dir_all(state_dir());
    let _ = std::fs::write(started_marker(current), b"");
    if p.previous.exists() {
        if let Err(e) = dispose(&p.previous) {
            eprintln!("update: previous app not moved to the trash ({e}); it is in {}", p.previous.display());
        }
    }
    remove_stage(&p.stage);
    clear_pending();
    Some(p.version)
}

/// Name of the previous app inside the staging folder during the exchange.
#[cfg(target_os = "macos")]
const PREVIOUS: &str = "previous";

/// Install a verified macOS package (`zip`) over the running `bundle`. The previous app is kept in
/// the staging folder next to it until the new version confirms its start ([`confirm_start`]); a
/// failure before the exchange leaves the running app untouched. `team`: required Developer ID team
/// ([`required_team`]).
#[cfg(target_os = "macos")]
pub fn install_macos(zip: &Path, bundle: &Path, version: &str, current: &str, team: Option<&str>) -> Result<Installed, InstallError> {
    let parent = bundle.parent().ok_or(InstallError::NotInstalled)?;
    if let Some(e) = location_problem(bundle) {
        return Err(e);
    }
    let zip_size = std::fs::metadata(zip).map_or(0, |m| m.len());
    check_space(parent, zip_size.saturating_mul(UNPACKED_FACTOR))?;
    let stage = staging_in(parent)?;
    let result = install_macos_staged(zip, bundle, &stage, version, current, team);
    let _ = std::fs::remove_dir_all(stage.join("unpacked"));
    if result.is_err() {
        clear_pending();
    }
    remove_stage(&stage);
    result
}

#[cfg(target_os = "macos")]
fn install_macos_staged(zip: &Path, bundle: &Path, stage: &Path, version: &str, current: &str, team: Option<&str>) -> Result<Installed, InstallError> {
    let fail = InstallError::Failed;
    let unpacked = stage.join("unpacked");
    std::fs::create_dir(&unpacked).map_err(|e| fail(e.to_string()))?;
    run("/usr/bin/ditto", &["-x".as_ref(), "-k".as_ref(), zip.as_os_str(), unpacked.as_os_str()]).map_err(|e| if e.contains("No space") { InstallError::NoSpace { need: 0, free: 0 } } else { fail(e) })?;
    let apps: Vec<PathBuf> = std::fs::read_dir(&unpacked).map_err(|e| fail(e.to_string()))?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "app")).collect();
    let [new] = apps.as_slice() else {
        return Err(fail("package does not contain exactly one app".into()));
    };
    let plist = new.join("Contents/Info.plist");
    if plist_value(&plist, "CFBundleIdentifier").as_deref() != Some(BUNDLE_ID) {
        return Err(fail("package has a different bundle identifier".into()));
    }
    let new_version = plist_value(&plist, "CFBundleShortVersionString").unwrap_or_default();
    if new_version != version {
        return Err(fail(format!("package version {new_version} instead of {version}")));
    }
    if !is_newer(&new_version, current) {
        return Err(fail(format!("package version {new_version} is not newer than {current}")));
    }
    verify_code(new, team).map_err(fail)?;
    let previous_dir = stage.join(PREVIOUS);
    std::fs::create_dir(&previous_dir).map_err(|e| fail(e.to_string()))?;
    let previous = previous_dir.join(bundle.file_name().unwrap_or_else(|| "graineo.app".as_ref()));
    // record first: if the process ends in the middle, the next start finds out what happened
    write_pending(&Pending { version: version.to_string(), from: current.to_string(), bundle: bundle.to_path_buf(), previous: previous.clone(), stage: stage.to_path_buf(), time: crate::now() }).map_err(fail)?;
    // exchange: one atomic swap (APFS), otherwise two moves with restore on failure
    match swap(new, bundle) {
        Ok(true) => {
            // `new` now holds the previous app
            if let Err(e) = std::fs::rename(new, &previous) {
                return Err(fail(format!("{e}; the previous app is in {}", new.display())));
            }
        }
        Ok(false) => {
            if let Err(e) = move_path(bundle, &previous) {
                return Err(if blocked(&e) {
                    InstallError::Blocked
                } else if e.contains("ermission") {
                    InstallError::NotWritable(bundle.display().to_string())
                } else {
                    fail(e)
                });
            }
            if let Err(e) = move_path(new, bundle) {
                let back = move_path(&previous, bundle);
                return Err(fail(match back {
                    Ok(()) => format!("{e} (previous app restored)"),
                    Err(b) => format!("{e}; previous app could not be restored: {b} – it is in {}", previous.display()),
                }));
            }
        }
        Err(e) => {
            let text = e.to_string();
            return Err(if e.kind() == std::io::ErrorKind::PermissionDenied && !blocked(&text) {
                InstallError::NotWritable(bundle.display().to_string())
            } else if blocked(&text) {
                InstallError::Blocked
            } else {
                fail(text)
            });
        }
    }
    let mut done = Installed { relaunch: bundle.to_path_buf(), previous: Some(previous), version: version.to_string(), ..Default::default() };
    if let Err(e) = run("/usr/bin/xattr", &["-dr".as_ref(), "com.apple.quarantine".as_ref(), bundle.as_os_str()]) {
        done.warnings.push(e);
    }
    let _ = std::fs::remove_file(zip); // own download
    Ok(done)
}

/// Install a verified Windows package next to the running `exe`.
pub fn install_windows(zip: &Path, exe: &Path) -> Result<Installed, InstallError> {
    let dir = exe.parent().ok_or(InstallError::NotInstalled)?;
    let zip_size = std::fs::metadata(zip).map_or(0, |m| m.len());
    check_space(dir, zip_size.saturating_mul(UNPACKED_FACTOR))?;
    let stage = staging_in(dir)?;
    let r = install_windows_staged(zip, exe, dir, &stage);
    let _ = std::fs::remove_dir_all(&stage);
    r
}

fn install_windows_staged(zip: &Path, exe: &Path, dir: &Path, stage: &Path) -> Result<Installed, InstallError> {
    let fail = InstallError::Failed;
    // bsdtar is part of Windows 10/11 and unpacks ZIP archives
    let out = command("tar").arg("-xf").arg(zip).arg("-C").arg(stage).output().map_err(|e| fail(format!("tar: {e}")))?;
    if !out.status.success() {
        return Err(fail(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let root = stage.join("graineo");
    let new_exe = root.join("graineo.exe");
    if !new_exe.is_file() {
        return Err(fail("package does not contain graineo.exe".into()));
    }
    let old_exe = dir.join("graineo.old.exe");
    let _ = std::fs::remove_file(&old_exe); // leftover of the previous update
    // a running exe can be renamed but not overwritten
    std::fs::rename(exe, &old_exe).map_err(|e| if e.kind() == std::io::ErrorKind::PermissionDenied { InstallError::NotWritable(dir.display().to_string()) } else { fail(e.to_string()) })?;
    if let Err(e) = std::fs::rename(&new_exe, exe) {
        let _ = std::fs::rename(&old_exe, exe);
        return Err(fail(format!("{e} (previous version restored)")));
    }
    let mut done = Installed { relaunch: exe.to_path_buf(), ..Default::default() };
    let new_data = root.join("spektrafilm");
    if new_data.is_dir() {
        let data = dir.join("spektrafilm");
        let old_data = dir.join("spektrafilm.old");
        let _ = std::fs::remove_dir_all(&old_data);
        let moved_old = !data.exists() || std::fs::rename(&data, &old_data).is_ok();
        if !moved_old || std::fs::rename(&new_data, &data).is_err() {
            if moved_old && !data.exists() {
                let _ = std::fs::rename(&old_data, &data);
            }
            done.warnings.push("film data not replaced".into());
        }
    }
    let _ = std::fs::remove_file(zip);
    Ok(done)
}

/// Leftovers of an update on Windows (`graineo.old.exe`, `spektrafilm.old` next to the exe),
/// removed at the next start.
pub fn leftovers() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else { return Vec::new() };
    ["graineo.old.exe", "spektrafilm.old"].iter().map(|n| dir.join(n)).filter(|p| p.exists()).collect()
}

/// Relaunch helper (macOS, `/bin/sh`): waits until the old process has ended, starts the new version
/// and – if a previous app is kept – waits up to three minutes for its start marker. If the new
/// version ends before it confirmed its start (crash, refused by macOS), the helper moves it into
/// `failed/` of the staging folder, puts the previous app back, leaves a marker and starts the previous
/// version. Arguments: pid, app, direct executable (test runs) or empty, start marker, previous app,
/// failed folder, rolled-back marker.
const RELAUNCH_SCRIPT: &str = r#"pid=$1; app=$2; direct=$3; ok=$4; prev=$5; failed=$6; rb=$7
while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
if [ -n "$direct" ]; then "$direct" & child=$!; else /usr/bin/open -n -W "$app" & child=$!; fi
[ -n "$ok" ] || exit 0
n=0
while [ "$n" -lt 900 ]; do
  [ -e "$ok" ] && break
  if ! kill -0 "$child" 2>/dev/null; then
    sleep 0.5
    [ -e "$ok" ] && break
    name=$(basename "$app")
    mkdir -p "$failed" && /bin/mv "$app" "$failed/$name" || exit 1
    if /bin/mv "$prev" "$app"; then : > "$rb"; else /bin/mv "$failed/$name" "$app"; exit 1; fi
    if [ -n "$direct" ]; then exec "$direct"; else exec /usr/bin/open -n "$app"; fi
  fi
  sleep 0.2
  n=$((n+1))
done
[ -z "$direct" ] && kill "$child" 2>/dev/null
exit 0"#;

/// Start the new version once this process has ended. macOS: a small shell waits for the PID and
/// watches the start ([`RELAUNCH_SCRIPT`]). Test runs (`GRAINEO_DATA_DIR`) start the executable
/// directly so that the environment – and with it the test data folder – is passed on; `open -n`
/// would start it with the real data.
pub fn relaunch(done: &Installed) -> Result<(), String> {
    let target = &done.relaunch;
    if cfg!(windows) {
        return command(&target.to_string_lossy()).spawn().map(|_| ()).map_err(|e| e.to_string());
    }
    let pid = std::process::id().to_string();
    let direct = if std::env::var_os("GRAINEO_DATA_DIR").is_some() {
        #[cfg(target_os = "macos")]
        let name = plist_value(&target.join("Contents/Info.plist"), "CFBundleExecutable").unwrap_or_else(|| "graineo".into());
        #[cfg(not(target_os = "macos"))]
        let name = String::from("graineo");
        target.join("Contents/MacOS").join(name).display().to_string()
    } else {
        String::new()
    };
    let (ok, previous, failed, rb) = match (&done.previous, read_pending()) {
        (Some(prev), Some(p)) if &p.previous == prev => (
            started_marker(&done.version).display().to_string(),
            prev.display().to_string(),
            p.stage.join("failed").display().to_string(),
            rolled_back_marker(&done.version).display().to_string(),
        ),
        _ => Default::default(),
    };
    command("/bin/sh")
        .arg("-c")
        .arg(RELAUNCH_SCRIPT)
        .arg("graineo-relaunch")
        .args([pid, target.display().to_string(), direct, ok, previous, failed, rb])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> (String, String) {
        keypair_from(7)
    }

    fn keypair_from(byte: u8) -> (String, String) {
        let seed = base64::engine::general_purpose::STANDARD.encode([byte; 32]);
        let public = public_key_of(&seed).unwrap();
        (seed, public)
    }

    fn sample(url: &str) -> Manifest {
        Manifest {
            version: "0.63.0".into(),
            date: "2026-11-05".into(),
            notes: "## New\n- Updates".into(),
            notes_de: Some("## Neu\n- Updates".into()),
            assets: vec![
                ("macos-universal".into(), Asset { url: format!("{url}/graineo-0.63.0-macOS.zip"), sha256: "ab".repeat(32), size: 1234 }),
                ("windows-x64".into(), Asset { url: format!("{url}/graineo-0.63.0-windows-x64.zip"), sha256: "cd".repeat(32), size: 99 }),
            ],
        }
    }

    #[test]
    fn signature_valid_tampered_and_wrong_key() {
        let (seed, public) = keypair();
        let data = manifest_json(&sample("https://example.org"));
        let sig = sign(data.as_bytes(), &seed).unwrap();
        assert_eq!(verify_signature(data.as_bytes(), &sig, &public), Ok(()));
        // one changed byte in the data
        let tampered = data.replace("0.63.0", "0.63.1");
        assert!(verify_signature(tampered.as_bytes(), &sig, &public).is_err());
        // changed signature
        let mut raw = base64::engine::general_purpose::STANDARD.decode(&sig).unwrap();
        raw[10] ^= 1;
        assert!(verify_signature(data.as_bytes(), &base64::engine::general_purpose::STANDARD.encode(raw), &public).is_err());
        // other key
        let other = public_key_of(&base64::engine::general_purpose::STANDARD.encode([8u8; 32])).unwrap();
        assert!(verify_signature(data.as_bytes(), &sig, &other).is_err());
        // garbage
        assert!(verify_signature(data.as_bytes(), "not base64!", &public).is_err());
        assert!(verify_signature(data.as_bytes(), &sig, "AAAA").is_err());
    }

    #[test]
    fn built_in_key_is_valid() {
        assert!(key_from_b64(PUBLIC_KEY).is_ok());
    }

    /// Both built-in keys are valid and distinct; a list signed with either one is accepted, one signed with a
    /// foreign key is rejected (the private keys never enter a test: the built-in public keys are swapped for
    /// generated ones in the same positions).
    #[test]
    fn current_and_next_key_are_both_trusted() {
        let next = NEXT_PUBLIC_KEY.expect("second key entered");
        assert!(key_from_b64(next).is_ok());
        assert_ne!(next, PUBLIC_KEY);
        assert_eq!(built_in_keys(), vec![PUBLIC_KEY, next]);
        let (seed_a, public_a) = keypair_from(1);
        let (seed_b, public_b) = keypair_from(2);
        let (seed_x, _) = keypair_from(3);
        let keys = vec![public_a, public_b];
        let data = manifest_json(&sample("https://example.org"));
        for seed in [&seed_a, &seed_b] {
            let sig = sign(data.as_bytes(), seed).unwrap();
            assert!(verify_any(data.as_bytes(), &sig, &keys).is_ok());
        }
        let foreign = sign(data.as_bytes(), &seed_x).unwrap();
        assert!(verify_any(data.as_bytes(), &foreign, &keys).is_err());
        let builtin: Vec<String> = built_in_keys().into_iter().map(str::to_string).collect();
        assert!(verify_any(data.as_bytes(), &foreign, &builtin).is_err());
    }

    #[test]
    fn evaluate_checks_signature_before_anything_else() {
        let (seed, public) = keypair();
        let data = manifest_json(&sample("https://example.org"));
        let sig = sign(data.as_bytes(), &seed).unwrap();
        assert!(matches!(evaluate(data.as_bytes(), &sig, &[public.clone()], "0.62.5", false), Ok(Check::Available(m)) if m.version == "0.63.0"));
        assert!(matches!(evaluate(data.as_bytes(), &sig, &[public.clone()], "0.63.0", false), Ok(Check::UpToDate { .. })));
        assert!(matches!(evaluate(data.as_bytes(), &sig, &[public.clone()], "1.0.0", false), Ok(Check::UpToDate { .. })));
        let tampered = data.replace("abab", "abac");
        assert!(evaluate(tampered.as_bytes(), &sig, &[public.clone()], "0.62.5", false).is_err());
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.63.0"), Some((0, 63, 0)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "1.2.3-beta", " ", "1..3", "+1.2.3", "1.2.-3"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
        assert!(is_newer("0.63.0", "0.62.5"));
        assert!(is_newer("0.62.10", "0.62.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.62.5", "0.62.5"));
        assert!(!is_newer("0.62.4", "0.62.5"));
        assert!(!is_newer("garbage", "0.0.1"));
        assert!(!is_newer("0.63.0", "garbage"));
    }

    #[test]
    fn prerelease_build_never_offers_update() {
        let (seed, public) = keypair();
        let data = manifest_json(&sample("https://example.org"));
        let sig = sign(data.as_bytes(), &seed).unwrap();
        // the same manifest offers 0.63.0 to a normal build ...
        assert!(matches!(evaluate(data.as_bytes(), &sig, &[public.clone()], "0.62.5", false), Ok(Check::Available(_))));
        // ... but never to a test build, not even with an invalid signature (it does not look at all)
        assert!(matches!(evaluate(data.as_bytes(), &sig, &[public.clone()], "0.62.5-leistung.1", false), Ok(Check::UpToDate { .. })));
        assert!(matches!(evaluate(data.as_bytes(), "garbage", &[public.clone()], "0.1.0-leistung.2", false), Ok(Check::UpToDate { .. })));
        assert!(is_prerelease("0.86.2-leistung.1"));
        assert!(!is_prerelease("0.86.2"));
    }

    #[test]
    fn missing_release_is_not_an_error() {
        assert!(not_found("https://github.com/graineo/graineo/releases/latest/download/latest.json: curl: (56) The requested URL returned error: 404"));
        assert!(not_found("x: curl: (22) The requested URL returned error: 404"));
        assert!(!not_found("x: curl: (6) Could not resolve host: github.com"));
        assert!(!not_found("x: curl: (22) The requested URL returned error: 500"));
    }

    #[test]
    fn rule_of_24_hours() {
        assert!(due(None, 1000));
        let t = 1_800_000_000;
        assert!(!due(Some(t), t));
        assert!(!due(Some(t), t + CHECK_INTERVAL - 1));
        assert!(due(Some(t), t + CHECK_INTERVAL));
        assert!(due(Some(t), t - 60)); // clock went backwards
    }

    #[test]
    fn parse_round_trip_and_rejections() {
        let m = sample("https://github.com/graineo/graineo/releases/download/v0.63.0");
        let j = manifest_json(&m);
        assert_eq!(parse_manifest(j.as_bytes(), false).unwrap(), m);
        assert_eq!(parse_manifest(j.as_bytes(), false).unwrap().notes_for(true), "## Neu\n- Updates");
        assert_eq!(parse_manifest(j.as_bytes(), false).unwrap().notes_for(false), "## New\n- Updates");
        // http only for the local test server
        let local = manifest_json(&sample("http://127.0.0.1:8765"));
        assert!(parse_manifest(local.as_bytes(), false).is_err());
        assert!(parse_manifest(local.as_bytes(), true).is_ok());
        let other_http = manifest_json(&sample("http://example.org"));
        assert!(parse_manifest(other_http.as_bytes(), true).is_err());
        let file = manifest_json(&sample("file:///etc"));
        assert!(parse_manifest(file.as_bytes(), true).is_err());
        // broken fields
        for (a, b) in [("\"0.63.0\"", "\"0.63\""), (&"ab".repeat(32) as &str, "xyz"), ("1234", "0")] {
            let broken = j.replacen(a, b, 1);
            assert!(parse_manifest(broken.as_bytes(), false).is_err(), "{a} → {b}");
        }
        assert!(parse_manifest(b"{}", false).is_err());
        assert!(parse_manifest(b"not json", false).is_err());
        assert!(!url_allowed("https://", false));
        assert!(!url_allowed("https://a b", false));
    }

    #[test]
    fn verify_file_checks_size_and_checksum() {
        let d = std::env::temp_dir().join(unique("graineo-update-test-"));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("pkg.zip");
        std::fs::write(&p, b"package").unwrap();
        let sha = crate::relocate::sha256(&p).unwrap();
        assert!(verify_file(&p, &Asset { url: String::new(), sha256: sha.clone(), size: 7 }).is_ok());
        assert!(verify_file(&p, &Asset { url: String::new(), sha256: sha.clone(), size: 8 }).is_err());
        std::fs::write(&p, b"pockage").unwrap();
        assert!(verify_file(&p, &Asset { url: String::new(), sha256: sha, size: 7 }).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Bundle with a real (ad hoc signed) executable, zipped like the release script does.
    #[cfg(target_os = "macos")]
    fn bundle(dir: &Path, id: &str, version: &str) -> PathBuf {
        let app = dir.join("graineo.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/graineo")).unwrap();
        let plist = format!(r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleShortVersionString</key><string>{version}</string><key>CFBundleExecutable</key><string>graineo</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#);
        std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
        run("/usr/bin/codesign", &["--force".as_ref(), "--deep".as_ref(), "--sign".as_ref(), "-".as_ref(), app.as_os_str()]).unwrap();
        app
    }

    /// Run the relaunch helper for a process that has already ended and wait for it.
    #[cfg(target_os = "macos")]
    fn run_helper(done: &Installed, direct: &Path) {
        let mut dead = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = dead.id().to_string();
        dead.wait().unwrap();
        let p = read_pending().unwrap();
        let st = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(RELAUNCH_SCRIPT)
            .arg("graineo-relaunch")
            .args([pid, done.relaunch.display().to_string(), direct.display().to_string(), started_marker(&done.version).display().to_string(), p.previous.display().to_string(), p.stage.join("failed").display().to_string(), rolled_back_marker(&done.version).display().to_string()])
            .status()
            .unwrap();
        assert!(st.success());
    }

    /// One test for the whole exchange (the record lies in the shared test data folder).
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_exchange_keeps_previous_until_confirmed_and_rolls_back() {
        let root = std::env::temp_dir().join(unique("graineo-update-install-"));
        let installed = root.join("apps");
        std::fs::create_dir_all(&installed).unwrap();
        let trash = root.join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        let dispose = |p: &Path| std::fs::rename(p, trash.join(unique("t-"))).map_err(|e| e.to_string());
        let zip_of = |id: &str, v: &str| {
            let b = root.join(unique("build-"));
            let app = bundle(&b, id, v);
            let zip = root.join(unique("pkg-") + ".zip");
            run("/usr/bin/ditto", &["-c".as_ref(), "-k".as_ref(), "--keepParent".as_ref(), app.as_os_str(), zip.as_os_str()]).unwrap();
            zip
        };
        let old = bundle(&installed, BUNDLE_ID, "0.0.1");
        let version = |p: &Path| plist_value(&p.join("Contents/Info.plist"), "CFBundleShortVersionString").unwrap();
        let only_app = || {
            let names: Vec<_> = std::fs::read_dir(&installed).unwrap().flatten().map(|e| e.file_name()).collect();
            assert_eq!(names, vec![std::ffi::OsString::from("graineo.app")], "nothing left next to the app");
        };
        // foreign bundle id, wrong version, not newer (downgrade), Developer ID of a team required but
        // only ad hoc signed: refused, the old app stays untouched, nothing left behind
        for (id, v, claimed, team) in [("org.example.other", "0.0.2", "0.0.2", None), (BUNDLE_ID, "0.0.3", "0.0.2", None), (BUNDLE_ID, "0.0.1", "0.0.1", None), (BUNDLE_ID, "0.0.0", "0.0.0", None), (BUNDLE_ID, "0.0.2", "0.0.2", Some("ABCDE12345"))] {
            let zip = zip_of(id, v);
            assert!(matches!(install_macos(&zip, &old, claimed, "0.0.1", team), Err(InstallError::Failed(_))), "{id} {v} {team:?}");
            assert_eq!(version(&old), "0.0.1");
            assert!(read_pending().is_none());
            only_app();
        }
        // a broken package
        let broken = root.join("broken.zip");
        std::fs::write(&broken, b"not a zip").unwrap();
        assert!(install_macos(&broken, &old, "0.0.2", "0.0.1", None).is_err());
        only_app();

        // proper package: exchanged, previous app kept until the new version confirms its start
        let zip = zip_of(BUNDLE_ID, "0.0.2");
        let done = install_macos(&zip, &old, "0.0.2", "0.0.1", None).unwrap();
        assert_eq!(version(&old), "0.0.2");
        let prev = done.previous.clone().unwrap();
        assert_eq!(version(&prev), "0.0.1");
        assert!(!zip.exists());
        assert_eq!(read_pending().unwrap().version, "0.0.2");
        // the new version runs: confirmed, previous app in the trash, nothing left next to the app
        assert_eq!(outcome("0.0.2", &dispose), Outcome::Started("0.0.2".into()));
        assert_eq!(confirm_start("0.0.2", &dispose), Some("0.0.2".into()));
        assert!(!prev.exists());
        assert!(read_pending().is_none());
        assert!(started_marker("0.0.2").exists());
        only_app();
        assert_eq!(std::fs::read_dir(&trash).unwrap().count(), 1);

        // the helper sees the start marker: no rollback
        let zip = zip_of(BUNDLE_ID, "0.0.3");
        let done = install_macos(&zip, &old, "0.0.3", "0.0.2", None).unwrap();
        std::fs::write(started_marker("0.0.3"), b"").unwrap();
        run_helper(&done, &old.join("Contents/MacOS/graineo"));
        assert_eq!(version(&old), "0.0.3");
        assert!(!rolled_back_marker("0.0.3").exists());
        assert_eq!(confirm_start("0.0.3", &dispose), Some("0.0.3".into()));
        only_app();

        // the new version ends at once without confirming (here: `true`): the helper puts the previous
        // app back and starts it; that one reports the failed update, skips it and trashes the copy
        let zip = zip_of(BUNDLE_ID, "0.0.4");
        let done = install_macos(&zip, &old, "0.0.4", "0.0.3", None).unwrap();
        assert_eq!(version(&old), "0.0.4");
        run_helper(&done, &old.join("Contents/MacOS/graineo"));
        assert_eq!(version(&old), "0.0.3", "previous app is back");
        assert!(rolled_back_marker("0.0.4").exists());
        assert_eq!(outcome("0.0.3", &dispose), Outcome::RolledBack("0.0.4".into()));
        assert_eq!(skipped_version().as_deref(), Some("0.0.4"));
        assert!(read_pending().is_none());
        only_app();
        let in_trash: Vec<_> = std::fs::read_dir(&trash).unwrap().flatten().map(|e| version(&e.path())).collect();
        assert!(in_trash.contains(&"0.0.4".to_string()), "{in_trash:?}");

        // a record of an exchange that never happened (process ended before it) is tidied up
        let stage = staging_in(&installed).unwrap();
        write_pending(&Pending { version: "0.0.9".into(), from: "0.0.3".into(), bundle: old.clone(), previous: stage.join("previous/graineo.app"), stage: stage.clone(), time: crate::now() }).unwrap();
        assert_eq!(outcome("0.0.3", &dispose), Outcome::Nothing);
        assert!(read_pending().is_none());
        only_app();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn second_installation_is_refused_while_one_runs() {
        let first = lock().unwrap();
        // flock is per open file: a second open (as by another process) is refused
        assert_eq!(lock().err(), Some(InstallError::Busy));
        drop(first);
        assert!(lock().is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn translocated_or_read_only_location_is_recognised() {
        assert_eq!(location_problem(Path::new("/private/var/folders/ab/T/AppTranslocation/1234/d/graineo.app")), Some(InstallError::NotMoved));
        assert_eq!(location_problem(&std::env::temp_dir()), None);
        // the sealed system volume is read-only, like a mounted disk image
        assert!(read_only_volume(Path::new("/System/Library")));
        assert!(!read_only_volume(&std::env::temp_dir()));
    }

    #[test]
    fn space_check() {
        let d = std::env::temp_dir();
        assert!(check_space(&d, 1).is_ok());
        assert!(matches!(check_space(&d, u64::MAX / 2), Err(InstallError::NoSpace { .. })));
    }

    #[test]
    fn signature_from_next_key_is_accepted() {
        let (seed, public) = keypair();
        let other = public_key_of(&base64::engine::general_purpose::STANDARD.encode([9u8; 32])).unwrap();
        let data = b"list";
        let sig = sign(data, &seed).unwrap();
        assert!(verify_any(data, &sig, &[other.clone(), public.clone()]).is_ok());
        assert!(verify_any(data, &sig, std::slice::from_ref(&other)).is_err());
        assert!(verify_any(data, &sig, &[]).is_err());
        assert!(built_in_keys().iter().all(|k| key_from_b64(k).is_ok()));
    }

    #[test]
    fn test_versions_in_the_bundle_count_as_test_builds() {
        assert!(is_prerelease("0.107.0-test"));
        assert!(parse_version("0.107.0-test").is_none());
        assert!(!is_newer("0.107.1-test", "0.107.0"));
    }

    #[test]
    fn repository_address_is_written_once() {
        assert!(DEFAULT_URL.starts_with(REPO_URL) && RELEASES_PAGE.starts_with(REPO_URL) && NEW_ISSUE_URL.starts_with(REPO_URL));
        assert!(blocked("mv: rename a to b: Operation not permitted"));
        assert!(!blocked("No such file"));
    }
}
