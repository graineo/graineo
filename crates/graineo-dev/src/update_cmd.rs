//! Release signing for the update mechanism (`library::update`).
//!
//! - `update-key [--out FILE]`: create a new Ed25519 key pair. The secret key goes to
//!   `~/.graineo-signing/update-ed25519.key` (mode 600) and is never overwritten; the public key
//!   is printed for `library::update::PUBLIC_KEY`.
//! - `update-sign FILE [--key KEYFILE]`: write `FILE.sig` (base64 signature over the bytes of FILE).
//! - `update-verify FILE [--pubkey BASE64]`: check `FILE.sig` (default: the keys built into the app).
//! - `update-manifest OUT --version V [--date D] [--notes MD] [--notes-de MD] --asset KEY FILE URL …`:
//!   write `latest.json` with size and SHA-256 of each package.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

fn default_key() -> Result<PathBuf> {
    let home = sysdirs::home().context("no home folder")?;
    Ok(home.join(".graineo-signing/update-ed25519.key"))
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub fn run(cmd: &str, args: &[String]) -> Result<()> {
    match cmd {
        "update-key" => key(option(args, "--out").map(PathBuf::from)),
        "update-sign" => {
            let file = args.first().context("usage: graineo-dev update-sign FILE [--key KEYFILE]")?;
            let key = option(args, "--key").map(PathBuf::from).map_or_else(default_key, Ok)?;
            let seed = std::fs::read_to_string(&key).with_context(|| format!("{}", key.display()))?;
            let data = std::fs::read(file).with_context(|| file.clone())?;
            let sig = library::update::sign(&data, &seed).map_err(anyhow::Error::msg)?;
            // self-check before anything is published
            let public = library::update::public_key_of(&seed).map_err(anyhow::Error::msg)?;
            library::update::verify_signature(&data, &sig, &public).map_err(anyhow::Error::msg)?;
            let out = format!("{file}.sig");
            std::fs::write(&out, format!("{sig}\n"))?;
            println!("{out}");
            if !library::update::built_in_keys().contains(&public.as_str()) {
                eprintln!("note: this key is not one of the keys built into the app (test key?)");
            }
            Ok(())
        }
        "update-verify" => {
            let file = args.first().context("usage: graineo-dev update-verify FILE [--pubkey BASE64]")?;
            let keys: Vec<String> = match option(args, "--pubkey") {
                Some(k) => vec![k],
                None => library::update::built_in_keys().into_iter().map(str::to_string).collect(),
            };
            let data = std::fs::read(file).with_context(|| file.clone())?;
            let sig = std::fs::read_to_string(format!("{file}.sig")).context("signature file missing")?;
            library::update::verify_any(&data, &sig, &keys).map_err(anyhow::Error::msg)?;
            let m = library::update::parse_manifest(&data, true).map_err(anyhow::Error::msg)?;
            println!("signature ok · version {} · {} packages", m.version, m.assets.len());
            Ok(())
        }
        "update-manifest" => manifest(args),
        _ => bail!("unknown command {cmd}"),
    }
}

fn key(out: Option<PathBuf>) -> Result<()> {
    let path = match out {
        Some(p) => p,
        None => default_key()?,
    };
    if path.exists() {
        bail!("{} already exists – not overwritten", path.display());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let seed = library::update::new_seed().map_err(anyhow::Error::msg)?;
    let public = library::update::public_key_of(&seed).map_err(anyhow::Error::msg)?;
    write_secret(&path, &seed)?;
    println!("secret key: {}", path.display());
    println!("public key: {public}");
    Ok(())
}

/// Create the file exclusively (never overwrite) with mode 600.
fn write_secret(path: &Path, seed: &str) -> Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).with_context(|| format!("{}", path.display()))?;
    f.write_all(format!("{seed}\n").as_bytes())?;
    Ok(())
}

fn manifest(args: &[String]) -> Result<()> {
    let usage = "usage: graineo-dev update-manifest OUT --version V [--date D] [--notes MD] [--notes-de MD] --asset KEY FILE URL …";
    let out = args.first().context(usage)?;
    let version = option(args, "--version").context(usage)?;
    anyhow::ensure!(library::update::parse_version(&version).is_some(), "invalid version {version}");
    let read = |k: &str| -> Result<Option<String>> {
        match option(args, k) {
            Some(p) if Path::new(&p).exists() => Ok(Some(std::fs::read_to_string(&p)?)),
            _ => Ok(None),
        }
    };
    let date = option(args, "--date").unwrap_or_else(|| {
        let o = std::process::Command::new("date").args(["-u", "+%Y-%m-%d"]).output();
        o.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    });
    let mut assets = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--asset" {
            let (key, file, url) = (args.get(i + 1).context(usage)?, args.get(i + 2).context(usage)?, args.get(i + 3).context(usage)?);
            let size = std::fs::metadata(file).with_context(|| file.clone())?.len();
            let sha256 = library::relocate::sha256(Path::new(file)).map_err(anyhow::Error::msg)?;
            assets.push((key.clone(), library::update::Asset { url: url.clone(), sha256, size }));
            i += 4;
        } else {
            i += 1;
        }
    }
    anyhow::ensure!(!assets.is_empty(), "{usage}");
    let m = library::update::Manifest { version, date, notes: read("--notes")?.unwrap_or_default(), notes_de: read("--notes-de")?, assets };
    let text = library::update::manifest_json(&m);
    // the file must be accepted by the app's own parser
    library::update::parse_manifest(text.as_bytes(), true).map_err(anyhow::Error::msg)?;
    std::fs::write(out, text)?;
    println!("{out}");
    Ok(())
}
