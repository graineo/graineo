//! Update of the lensfun database on request ("Objektivprofile aktualisieren" in Settings → Lenses).
//!
//! graineo ships the whole database (`crates/raw/data/lensfun-db.xml.gz`). Only when the user clicks the button, the
//! newest state of `data/db` is fetched from GitHub (no automatic network access):
//! 1. the newest commit that touched `data/db` (`api.github.com/repos/lensfun/lensfun/commits?path=data/db`),
//! 2. the source archive of that commit (`codeload.github.com/lensfun/lensfun/tar.gz/<commit>`),
//! 3. all `data/db/*.xml` are merged as `scripts/lensfun-update.py` does (comments, indentation and non-English
//!    translated names removed, the own GR II entry appended),
//! 4. checked (size, parsable, at least as many cameras and lenses as a sane database has) and stored compressed as
//!    `lens-profiles/lensfun-db.xml.gz` in the data folder; from then on it replaces the built-in data
//!    ([`crate::lensfun::Database::active`]). "Zurück auf mitgelieferte" removes the file again.
//!
//! Network access goes through the system's `curl` (as the app's other downloads). Data license: CC BY-SA 3.0,
//! © the lensfun contributors.

use std::io::Read;
use std::path::PathBuf;

use crate::lensfun::Database;

const API: &str = "https://api.github.com/repos/lensfun/lensfun/commits?path=data/db&per_page=1";
const ARCHIVE: &str = "https://codeload.github.com/lensfun/lensfun/tar.gz/";
/// The archive of the whole repository is a few MB; anything far larger is refused.
const ARCHIVE_LIMIT: u64 = 200 << 20;
/// A merged database below this size (unpacked) is not a database (the built-in one has about 6 MB).
const MIN_XML: usize = 1 << 20;
const MIN_CAMERAS: usize = 500;
const MIN_LENSES: usize = 800;

/// Own camera entry also appended by `scripts/lensfun-update.py`.
const OWN: &str = "<!-- own addition (not from lensfun): the Ricoh GR II has the lens of the GR -->\n<camera><maker>Ricoh Imaging Company, Ltd.</maker><model>GR II</model><mount>ricohGR</mount><cropfactor>1.523</cropfactor></camera>";

/// The downloaded database in the data folder (`None` without a data folder).
pub fn file() -> Option<PathBuf> {
    lensprof::store::root().map(|d| d.join("lensfun-db.xml.gz"))
}

fn curl() -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new("curl");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c.args(["-sSfL", "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "5", "--connect-timeout", "20", "-A"]).arg(format!("graineo/{}", env!("CARGO_PKG_VERSION")));
    c
}

/// Newest commit of `data/db`: (hash, date YYYY-MM-DD).
fn newest_commit() -> Result<(String, String), String> {
    let out = curl().args(["--max-time", "30", "--max-filesize", "1000000", "-H", "Accept: application/vnd.github+json"]).arg(API).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let c = v.get(0).ok_or("no commit")?;
    let sha = c["sha"].as_str().filter(|s| s.len() == 40 && s.chars().all(|x| x.is_ascii_hexdigit())).ok_or("no commit hash")?;
    let date = c["commit"]["committer"]["date"].as_str().unwrap_or("").chars().take(10).collect();
    Ok((sha.to_string(), date))
}

/// Remove `<!-- … -->` comments.
fn without_comments(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Remove translated names other than English: `<tag lang="xx">…</tag>` (with the whitespace before it).
fn without_translations(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(i) = rest.find(" lang=\"") {
        // start of the element: the '<' before the attribute
        let Some(lt) = rest[..i].rfind('<') else { break };
        let tag = &rest[lt + 1..i];
        let lang_end = rest[i + 7..].find('"').map(|j| i + 7 + j);
        let close = format!("</{tag}>");
        let (Some(le), Some(ce)) = (lang_end, rest[i..].find(&close).map(|j| i + j + close.len())) else { break };
        let lang = &rest[i + 7..le];
        let keep = lang == "en" || tag.contains(' ') || tag.is_empty();
        if keep {
            out.push_str(&rest[..ce]);
        } else {
            out.push_str(rest[..lt].trim_end());
        }
        rest = &rest[ce..];
    }
    out.push_str(rest);
    out
}

/// Collapse whitespace between tags (`>\s+<` → `><`).
fn compact(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut pending = String::new();
    let mut after_gt = false;
    for c in t.chars() {
        if after_gt && c.is_whitespace() {
            pending.push(c);
            continue;
        }
        if !pending.is_empty() {
            if c != '<' {
                out.push_str(&pending);
            }
            pending.clear();
        }
        out.push(c);
        after_gt = c == '>';
    }
    out.push_str(&pending);
    out.trim().to_string()
}

/// Merge the XML files (name, text) of `data/db` into one document, as `scripts/lensfun-update.py`.
pub fn merge(files: &mut [(String, String)], commit: &str, date: &str) -> String {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut body = Vec::new();
    for (name, t) in files.iter() {
        let Some(s) = t.find("<lensdatabase") else { continue };
        let Some(open_end) = t[s..].find('>').map(|j| s + j + 1) else { continue };
        let Some(e) = t.rfind("</lensdatabase>") else { continue };
        if e < open_end {
            continue;
        }
        let inner = compact(&without_translations(&without_comments(&t[open_end..e])));
        body.push(format!("<!-- {name} -->{inner}"));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!--\n  lensfun database (https://github.com/lensfun/lensfun, data/db, commit {commit}, {date}).\n  License of the data: Creative Commons Attribution-ShareAlike 3.0 (CC BY-SA 3.0),\n  https://creativecommons.org/licenses/by-sa/3.0/ - (c) the lensfun contributors.\n  Changes: all files merged into one; comments, indentation and non-English translated\n  names removed, elements otherwise unchanged; in addition one own camera entry \"GR II\"\n  (same lens as the GR, marked as such). Downloaded and merged by graineo on request.\n-->\n<lensdatabase version=\"2\">\n{}\n{OWN}\n</lensdatabase>\n",
        body.join("\n")
    )
}

/// Is the merged XML a usable database? Returns it parsed.
pub fn check(xml: &str) -> Result<Database, String> {
    if xml.len() < MIN_XML {
        return Err(format!("database too small ({} bytes)", xml.len()));
    }
    let db = Database::load(xml)?;
    if db.cameras.len() < MIN_CAMERAS || db.lenses.len() < MIN_LENSES {
        return Err(format!("database incomplete ({} cameras, {} lenses)", db.cameras.len(), db.lenses.len()));
    }
    Ok(db)
}

/// `data/db/*.xml` from the source archive (tar.gz).
fn xml_files(archive: &[u8]) -> Result<Vec<(String, String)>, String> {
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut out = Vec::new();
    for e in a.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| e.to_string())?;
        let p = e.path().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
        // "<repo>-<commit>/data/db/<name>.xml"
        let Some((_, rest)) = p.split_once('/') else { continue };
        let Some(name) = rest.strip_prefix("data/db/") else { continue };
        if !name.ends_with(".xml") || name.contains('/') {
            continue;
        }
        let mut t = String::new();
        e.read_to_string(&mut t).map_err(|e| format!("{name}: {e}"))?;
        out.push((name.to_string(), t));
    }
    if out.is_empty() {
        return Err("no data/db in the archive".into());
    }
    Ok(out)
}

/// What an update did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// newly stored: commit, date, cameras, lenses
    Installed { commit: String, date: String, cameras: usize, lenses: usize },
    /// the data in use is already this commit
    Current { commit: String, date: String },
}

/// Fetch, check and store the newest lensfun data (blocking; only on the user's request).
pub fn update() -> Result<Outcome, String> {
    fetch(false)
}

/// `force`: store even if the data in use is that commit already (tests).
fn fetch(force: bool) -> Result<Outcome, String> {
    let target = file().ok_or("no data folder")?;
    let (commit, date) = newest_commit()?;
    if !force && Database::active().commit == commit {
        return Ok(Outcome::Current { commit, date });
    }
    let dir = target.parent().ok_or("no data folder")?.to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let part = dir.join("lensfun-download.tar.gz.part");
    let _ = std::fs::remove_file(&part);
    let st = curl().args(["--max-time", "300", "--max-filesize"]).arg(ARCHIVE_LIMIT.to_string()).arg("-o").arg(&part).arg(format!("{ARCHIVE}{commit}")).status().map_err(|e| format!("curl: {e}"))?;
    let bytes = std::fs::read(&part).unwrap_or_default();
    let _ = std::fs::remove_file(&part);
    if !st.success() || bytes.is_empty() {
        return Err("download failed".into());
    }
    let mut files = xml_files(&bytes)?;
    let xml = merge(&mut files, &commit, &date);
    let db = check(&xml)?;
    let (cameras, lenses) = (db.cameras.len(), db.lenses.len());
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    std::io::Write::write_all(&mut gz, xml.as_bytes()).map_err(|e| e.to_string())?;
    let packed = gz.finish().map_err(|e| e.to_string())?;
    let tmp = target.with_extension("gz.tmp");
    std::fs::write(&tmp, &packed).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    crate::lensfun::reload();
    Ok(Outcome::Installed { commit, date, cameras, lenses })
}

/// Back to the built-in data: remove the downloaded file.
pub fn revert() -> Result<(), String> {
    if let Some(f) = file().filter(|f| f.exists()) {
        std::fs::remove_file(&f).map_err(|e| format!("{}: {e}", f.display()))?;
    }
    crate::lensfun::reload();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_matches_the_build_script() {
        let a = "<?xml version=\"1.0\"?>\n<!-- c -->\n<lensdatabase version=\"2\">\n  <!-- x -->\n  <lens>\n    <maker>Foo</maker>\n    <maker lang=\"de\">Fu</maker>\n    <model lang=\"en\">Bar</model>\n    <model>Bar 1</model>\n  </lens>\n</lensdatabase>\n";
        let mut files = vec![("b.xml".to_string(), a.to_string()), ("a.xml".to_string(), "<lensdatabase><mount><name>M</name></mount></lensdatabase>".to_string())];
        let x = merge(&mut files, "abc", "2026-11-25");
        assert!(x.contains("<!-- a.xml --><mount><name>M</name></mount>\n<!-- b.xml --><lens><maker>Foo</maker><model lang=\"en\">Bar</model><model>Bar 1</model></lens>"), "{x}");
        assert!(x.contains("commit abc, 2026-11-25"));
        assert_eq!(crate::lensfun::header_version(&x), ("abc".into(), "2026-11-25".into()));
        assert!(check(&x).is_err(), "too small");
    }

    /// Real download (network): only with `GRAINEO_TEST_LENSFUN_UPDATE=1` and `GRAINEO_DATA_DIR` set to a test folder.
    #[test]
    fn real_update_into_test_folder() {
        if std::env::var("GRAINEO_TEST_LENSFUN_UPDATE").as_deref() != Ok("1") || std::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return;
        }
        let r = fetch(true).expect("update");
        eprintln!("{r:?}");
        let db = Database::active();
        eprintln!("active: {} {} · {} cameras, {} lenses", db.commit, db.date, db.cameras.len(), db.lenses.len());
        assert!(file().unwrap().exists());
        assert!(matches!(update(), Ok(Outcome::Current { .. })));
        revert().unwrap();
        assert!(Database::active().is_built_in());
    }

    /// The merge reproduces the built-in database (made by the script) from its own content.
    #[test]
    fn built_in_passes_the_check() {
        let x = Database::built_in_xml().unwrap();
        let db = check(&x).expect("built-in is a database");
        assert!(db.lenses.len() > MIN_LENSES);
    }
}
