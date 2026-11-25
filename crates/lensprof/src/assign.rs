//! Assignment of own profiles: rules in `lens-profiles/assignments.json` and the choice of the profile for a photo.
//!
//! A photo carries its own assignment (`RawAdjust.lens_profile`, `"id@version"`, `""` = explicitly none). Without
//! one, the first matching rule counts ("all photos of this camera without a lens name", "this lens name").
//! Rules are looked up live (cached), so a new rule works at once for every photo it matches.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::names::{first_word, model_matches, norm};
use crate::own::OwnProfile;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    /// All photos of a camera whose file names no lens (manual lens, adapter).
    CameraWithoutLens { maker: String, model: String, profile: String },
    /// All photos of a camera with this lens name.
    LensName { maker: String, model: String, lens: String, profile: String },
}

impl Rule {
    pub fn profile(&self) -> &str {
        match self {
            Rule::CameraWithoutLens { profile, .. } | Rule::LensName { profile, .. } => profile,
        }
    }

    pub fn maker(&self) -> &str {
        match self {
            Rule::CameraWithoutLens { maker, .. } | Rule::LensName { maker, .. } => maker,
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Rule::CameraWithoutLens { model, .. } | Rule::LensName { model, .. } => model,
        }
    }

    /// Lens name the rule is for (`None` = without a lens name).
    pub fn lens(&self) -> Option<&str> {
        match self {
            Rule::CameraWithoutLens { .. } => None,
            Rule::LensName { lens, .. } => Some(lens),
        }
    }

    /// Same target (camera and lens), whatever the profile.
    pub fn same_target(&self, other: &Rule) -> bool {
        first_word(self.maker()) == first_word(other.maker())
            && model_matches(self.model(), other.model())
            && self.lens().is_some() == other.lens().is_some()
            && self.lens().map(norm).unwrap_or_default() == other.lens().map(norm).unwrap_or_default()
    }

    /// Does the rule apply to a photo of this camera and lens name?
    pub fn matches(&self, maker: &str, model: &str, lens: Option<&str>) -> bool {
        let has_lens = lens.map(str::trim).is_some_and(|l| !l.is_empty());
        // an empty maker is a wildcard (the library often names cameras without the maker, "ILCE-7M3")
        (self.maker().trim().is_empty() || first_word(self.maker()) == first_word(maker))
            && model_matches(self.model(), model)
            && match self {
                Rule::CameraWithoutLens { .. } => !has_lens,
                Rule::LensName { lens: l, .. } => has_lens && norm(l) == norm(lens.unwrap_or("")),
            }
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    #[serde(default)]
    format: u32,
    #[serde(default)]
    rules: Vec<Rule>,
}

fn path() -> Option<PathBuf> {
    crate::store::root().map(|d| d.join("assignments.json"))
}

struct Cache {
    path: PathBuf,
    mtime: Option<SystemTime>,
    generation: u64,
    rules: Arc<Vec<Rule>>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// All rules (read again when the file changes).
pub fn rules() -> Arc<Vec<Rule>> {
    let Some(p) = path() else { return Arc::new(Vec::new()) };
    let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
    let generation = crate::store::generation();
    if let Ok(c) = CACHE.lock() {
        if let Some(c) = c.as_ref().filter(|c| c.path == p && c.mtime == mtime && c.generation == generation) {
            return c.rules.clone();
        }
    }
    let mut rules: Vec<Rule> = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<File>(&t).ok()).map(|f| f.rules).unwrap_or_default();
    // rules written up to 1.5.4 name a fixed lens with the old suffix (see `legacy_names::lens`)
    for r in rules.iter_mut() {
        if let Rule::LensName { lens, .. } = r {
            if let std::borrow::Cow::Owned(n) = legacy_names::lens::fixed_lens_name(lens) {
                *lens = n;
            }
        }
    }
    let rules = Arc::new(rules);
    if let Ok(mut c) = CACHE.lock() {
        *c = Some(Cache { path: p, mtime, generation, rules: rules.clone() });
    }
    rules
}

pub fn save_rules(rules: &[Rule]) -> std::io::Result<()> {
    let p = path().ok_or_else(|| std::io::Error::other("kein Benutzerordner"))?;
    crate::store::write_atomic(&p, &File { format: 1, rules: rules.to_vec() })?;
    crate::store::bump_generation();
    Ok(())
}

/// Set a rule; an existing rule for the same target is replaced.
pub fn set_rule(rule: Rule) -> std::io::Result<()> {
    let mut v: Vec<Rule> = rules().as_ref().clone();
    v.retain(|r| !r.same_target(&rule));
    v.push(rule);
    save_rules(&v)
}

/// Remove the rule for a target; `true` if there was one.
pub fn remove_rule(target: &Rule) -> std::io::Result<bool> {
    let mut v: Vec<Rule> = rules().as_ref().clone();
    let n = v.len();
    v.retain(|r| !r.same_target(target));
    let changed = v.len() != n;
    if changed {
        save_rules(&v)?;
    }
    Ok(changed)
}

/// Remove all rules that point to profile `id` (it is deleted).
pub fn remove_rules_of(id: &str) -> std::io::Result<()> {
    let mut v: Vec<Rule> = rules().as_ref().clone();
    let n = v.len();
    v.retain(|r| r.profile().rsplit_once('@').map_or(r.profile(), |(i, _)| i) != id);
    if v.len() != n {
        save_rules(&v)?;
    }
    Ok(())
}

/// Profile of the rule for a photo (`"id@version"`), if one matches. A lens-name rule wins over "without lens name".
pub fn rule_for(maker: &str, model: &str, lens: Option<&str>) -> Option<String> {
    let r = rules();
    r.iter().filter(|r| r.matches(maker, model, lens)).max_by_key(|r| matches!(r, Rule::LensName { .. })).map(|r| r.profile().to_string())
}

/// Effective assignment of a photo: its own (`Some`, `""` = none) before the rules.
pub fn effective(maker: &str, model: &str, lens: Option<&str>, pinned: Option<&str>) -> Option<String> {
    match pinned {
        Some(p) => Some(p.to_string()),
        None => rule_for(maker, model, lens),
    }
}

/// Where an assignment comes from, for the display.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// no assignment: the automatic choice (lens name, Adobe, camera, lensfun) applies
    Automatic,
    /// explicitly no own profile
    None,
    /// assigned to the photo
    Photo(String),
    /// by a rule
    Rule(String),
}

pub fn source(maker: &str, model: &str, lens: Option<&str>, pinned: Option<&str>) -> Source {
    match pinned {
        Some("") => Source::None,
        Some(p) => Source::Photo(p.to_string()),
        None => rule_for(maker, model, lens).map_or(Source::Automatic, Source::Rule),
    }
}

/// Own profiles of the same maker and camera model (a flat field depends on the sensor).
pub fn candidates(maker: &str, model: &str) -> Vec<Arc<OwnProfile>> {
    crate::store::list().into_iter().filter(|p| (maker.trim().is_empty() || first_word(&p.maker) == first_word(maker)) && model_matches(&p.model, model)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{lock, scratch};

    fn rule(kind_lens: Option<&str>, profile: &str) -> Rule {
        match kind_lens {
            None => Rule::CameraWithoutLens { maker: "SIGMA".into(), model: "SIGMA sd Quattro".into(), profile: profile.into() },
            Some(l) => Rule::LensName { maker: "SIGMA".into(), model: "SIGMA sd Quattro".into(), lens: l.into(), profile: profile.into() },
        }
    }

    #[test]
    fn rules_match_and_order() {
        let _l = lock();
        scratch("assign-rules");
        assert!(rules().is_empty());
        set_rule(rule(None, "aaaaaaaaaaaa@1")).unwrap();
        set_rule(rule(Some("----"), "bbbbbbbbbbbb@2")).unwrap();
        assert_eq!(rule_for("SIGMA", "sd Quattro", None).as_deref(), Some("aaaaaaaaaaaa@1"));
        assert_eq!(rule_for("SIGMA", "sd Quattro", Some("  ")).as_deref(), Some("aaaaaaaaaaaa@1"));
        assert_eq!(rule_for("SIGMA", "sd Quattro", Some("----")).as_deref(), Some("bbbbbbbbbbbb@2"));
        assert_eq!(rule_for("SIGMA", "sd Quattro", Some("FE 50mm")), None);
        assert_eq!(rule_for("Canon", "EOS RP", None), None);
        // wildcard maker
        set_rule(Rule::LensName { maker: String::new(), model: "ILCE-7M3".into(), lens: "FE 50mm".into(), profile: "eeeeeeeeeeee@1".into() }).unwrap();
        assert_eq!(rule_for("SONY", "ILCE-7M3", Some("FE 50mm")).as_deref(), Some("eeeeeeeeeeee@1"));
        assert!(remove_rule(&Rule::LensName { maker: String::new(), model: "ILCE-7M3".into(), lens: "FE 50mm".into(), profile: String::new() }).unwrap());
        // replacing the rule for the same target
        set_rule(rule(None, "cccccccccccc@3")).unwrap();
        assert_eq!(rules().len(), 2);
        assert_eq!(rule_for("SIGMA", "SIGMA sd Quattro", None).as_deref(), Some("cccccccccccc@3"));
        // the photo's own choice wins, "" switches off
        assert_eq!(effective("SIGMA", "sd Quattro", None, Some("dddddddddddd@1")).as_deref(), Some("dddddddddddd@1"));
        assert_eq!(effective("SIGMA", "sd Quattro", None, Some("")).as_deref(), Some(""));
        assert_eq!(source("SIGMA", "sd Quattro", None, Some("")), Source::None);
        assert_eq!(source("Canon", "EOS RP", None, None), Source::Automatic);
        remove_rules_of("cccccccccccc").unwrap();
        assert_eq!(rule_for("SIGMA", "sd Quattro", None), None);
        assert!(remove_rule(&rule(Some("----"), "")).unwrap());
        assert!(rules().is_empty());
    }
}
