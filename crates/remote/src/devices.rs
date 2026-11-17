//! Paired devices and pairing codes.
//!
//! A device proves itself with a random 256-bit token kept in an `HttpOnly` cookie. graineo
//! stores only the SHA-256 hash of the token (file `devices.json` in the remote folder, mode
//! 0600), so the file alone does not let anyone in. Every device can be revoked on its own and
//! expires after the chosen lifetime. A device is paired with a short one-time code shown in
//! graineo (valid a few minutes, a few attempts only).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What a device may do.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// browse the library and look at previews
    Read,
    /// additionally rate, flag, label and use the simple develop sliders
    Write,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Device {
    /// short public id (not secret), shown in graineo and in the access log
    pub id: String,
    pub name: String,
    /// SHA-256 of the token, hex
    token_hash: String,
    /// value the page must send in the `X-Graineo-CSRF` header with every write
    pub csrf: String,
    pub role: Role,
    pub created: i64,
    pub last_seen: i64,
    /// unix time; 0 = never expires
    pub expires: i64,
}

impl Device {
    pub fn expired(&self, now: i64) -> bool {
        self.expires != 0 && now >= self.expires
    }
}

struct Pairing {
    code: String,
    role: Role,
    expires: i64,
    attempts: u32,
    /// wrong attempts per address: one address alone cannot burn the code shown to the owner
    per_peer: std::collections::HashMap<std::net::IpAddr, u32>,
}

/// Wrong codes per address for one code (that address gets no further try at it).
pub const PAIR_ATTEMPTS: u32 = 5;
/// Wrong codes from all addresses before the current code is invalidated.
pub const PAIR_ATTEMPTS_TOTAL: u32 = 15;
/// Lifetime of a pairing code in seconds.
pub const PAIR_SECONDS: i64 = 300;
/// Most devices that can be paired at the same time.
pub const MAX_DEVICES: usize = 20;

pub struct Devices {
    file: PathBuf,
    list: Mutex<Vec<Device>>,
    pairing: Mutex<Option<Pairing>>,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).expect("system random source");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|x| format!("{x:02x}")).collect()
}

/// Comparison without an early exit (timing does not reveal how many characters matched).
pub fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Alphabet of pairing codes: no 0/O, 1/I/L (easy to type on a tablet).
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

fn new_code() -> String {
    // 8 characters from 31 → about 39.6 bits; with 5 attempts per code and 5 minutes lifetime
    // guessing is hopeless
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("system random source");
    let s: String = b.iter().map(|x| CODE_ALPHABET[(*x as usize) % CODE_ALPHABET.len()] as char).collect();
    format!("{}-{}", &s[..4], &s[4..])
}

fn normalize_code(s: &str) -> String {
    let c: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
    if c.len() == 8 {
        format!("{}-{}", &c[..4], &c[4..])
    } else {
        c
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    /// no code is active (none shown, expired or used up)
    NoCode,
    Wrong,
    TooManyDevices,
}

impl Devices {
    pub fn load(dir: &Path) -> Devices {
        let file = dir.join("devices.json");
        let list: Vec<Device> = fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Devices { file, list: Mutex::new(list), pairing: Mutex::new(None) }
    }

    fn save(&self, list: &[Device]) {
        let tmp = self.file.with_extension("tmp");
        let text = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".into());
        let res = (|| -> std::io::Result<()> {
            if let Some(d) = self.file.parent() {
                fs::create_dir_all(d)?;
            }
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &self.file)
        })();
        if let Err(e) = res {
            eprintln!("remote: saving devices failed: {e}");
        }
    }

    pub fn list(&self) -> Vec<Device> {
        let now = now_unix();
        let mut l = self.list.lock().unwrap();
        let before = l.len();
        l.retain(|d| !d.expired(now));
        if l.len() != before {
            self.save(&l);
        }
        l.clone()
    }

    /// Shows a new pairing code (replaces an older one).
    pub fn start_pairing(&self, role: Role) -> (String, i64) {
        let code = new_code();
        let expires = now_unix() + PAIR_SECONDS;
        *self.pairing.lock().unwrap() = Some(Pairing { code: code.clone(), role, expires, attempts: 0, per_peer: Default::default() });
        (code, expires)
    }

    pub fn cancel_pairing(&self) {
        *self.pairing.lock().unwrap() = None;
    }

    /// Current code and its expiry, if one is active.
    pub fn pairing(&self) -> Option<(String, i64)> {
        let mut p = self.pairing.lock().unwrap();
        if let Some(x) = p.as_ref() {
            if x.expires <= now_unix() || x.attempts >= PAIR_ATTEMPTS_TOTAL {
                *p = None;
                return None;
            }
            return Some((x.code.clone(), x.expires));
        }
        None
    }

    /// Checks a code and creates a device. Returns the device and its token (shown only once,
    /// it goes into the cookie). The code is used up on success.
    pub fn pair(&self, code: &str, name: &str, lifetime_days: i64, peer: std::net::IpAddr) -> Result<(Device, String), PairError> {
        let role = {
            let mut p = self.pairing.lock().unwrap();
            let Some(x) = p.as_mut() else { return Err(PairError::NoCode) };
            if x.expires <= now_unix() || x.attempts >= PAIR_ATTEMPTS_TOTAL {
                *p = None;
                return Err(PairError::NoCode);
            }
            let mine = x.per_peer.get(&peer).copied().unwrap_or(0);
            if mine >= PAIR_ATTEMPTS {
                return Err(PairError::NoCode);
            }
            if !same(&normalize_code(code), &x.code) {
                x.attempts += 1;
                *x.per_peer.entry(peer).or_insert(0) += 1;
                if x.attempts >= PAIR_ATTEMPTS_TOTAL {
                    *p = None;
                }
                return Err(PairError::Wrong);
            }
            let role = x.role;
            *p = None;
            role
        };
        let mut l = self.list.lock().unwrap();
        let now = now_unix();
        l.retain(|d| !d.expired(now));
        if l.len() >= MAX_DEVICES {
            return Err(PairError::TooManyDevices);
        }
        let token = random_hex(32);
        let name: String = name.chars().filter(|c| !c.is_control()).take(60).collect::<String>().trim().to_string();
        let device = Device {
            id: random_hex(4),
            name: if name.is_empty() { "Browser".into() } else { name },
            token_hash: hash(&token),
            csrf: random_hex(16),
            role,
            created: now,
            last_seen: now,
            expires: if lifetime_days > 0 { now + lifetime_days * 86_400 } else { 0 },
        };
        l.push(device.clone());
        self.save(&l);
        Ok((device, token))
    }

    /// The device of a token, if it is valid. Updates "last seen" (saved at most every 10 min).
    pub fn check(&self, token: &str) -> Option<Device> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let h = hash(token);
        let now = now_unix();
        let mut l = self.list.lock().unwrap();
        let mut found = None;
        let mut dirty = false;
        for d in l.iter_mut() {
            // compare every entry (no early exit)
            if same(&d.token_hash, &h) && !d.expired(now) {
                if now - d.last_seen > 600 {
                    d.last_seen = now;
                    dirty = true;
                }
                found = Some(d.clone());
            }
        }
        if dirty {
            self.save(&l);
        }
        found
    }

    pub fn revoke(&self, id: &str) -> bool {
        let mut l = self.list.lock().unwrap();
        let before = l.len();
        l.retain(|d| d.id != id);
        let changed = l.len() != before;
        if changed {
            self.save(&l);
        }
        changed
    }

    pub fn revoke_all(&self) {
        let mut l = self.list.lock().unwrap();
        l.clear();
        self.save(&l);
    }

    pub fn set_role(&self, id: &str, role: Role) {
        let mut l = self.list.lock().unwrap();
        if let Some(d) = l.iter_mut().find(|d| d.id == id) {
            d.role = role;
        }
        self.save(&l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(k: u8) -> std::net::IpAddr {
        std::net::IpAddr::from([192, 168, 1, k])
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-remote-dev-{}", random_hex(4)));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pairing_is_one_time_and_limited() {
        let dir = tmp();
        let devs = Devices::load(&dir);
        assert_eq!(devs.pair("ABCD-EFGH", "x", 30, ip(1)).unwrap_err(), PairError::NoCode);
        let (code, _) = devs.start_pairing(Role::Write);
        for _ in 0..PAIR_ATTEMPTS - 1 {
            assert_eq!(devs.pair("AAAA-AAAA", "x", 30, ip(1)).unwrap_err(), PairError::Wrong);
        }
        // the right code still works after fewer than the maximum attempts, written any way
        let (dev, token) = devs.pair(&code.to_lowercase().replace('-', " "), "iPad", 30, ip(1)).unwrap();
        assert_eq!(dev.role, Role::Write);
        assert_eq!(devs.pair(&code, "again", 30, ip(1)).unwrap_err(), PairError::NoCode, "used up");
        assert_eq!(devs.check(&token).unwrap().id, dev.id);
        // file holds only the hash
        let text = fs::read_to_string(dir.join("devices.json")).unwrap();
        assert!(!text.contains(&token));
        // reload from disk
        let again = Devices::load(&dir);
        assert!(again.check(&token).is_some());
        assert!(again.revoke(&dev.id));
        assert!(again.check(&token).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_codes_burn_the_code() {
        let dir = tmp();
        let devs = Devices::load(&dir);
        let (code, _) = devs.start_pairing(Role::Read);
        // one address alone: locked out of this code, but the code stays for the owner's device
        for _ in 0..PAIR_ATTEMPTS {
            let _ = devs.pair("ZZZZ-ZZZZ", "x", 30, ip(1));
        }
        assert_eq!(devs.pair(&code, "x", 30, ip(1)).unwrap_err(), PairError::NoCode);
        assert!(devs.pairing().is_some());
        // many addresses: the code is burnt
        for k in 2..10 {
            for _ in 0..PAIR_ATTEMPTS {
                let _ = devs.pair("ZZZZ-ZZZZ", "x", 30, ip(k));
            }
        }
        assert!(devs.pairing().is_none());
        assert_eq!(devs.pair(&code, "x", 30, ip(42)).unwrap_err(), PairError::NoCode);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        let dir = tmp();
        let devs = Devices::load(&dir);
        assert!(devs.check("").is_none());
        assert!(devs.check(&"g".repeat(64)).is_none());
        assert!(devs.check(&"0".repeat(64)).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
