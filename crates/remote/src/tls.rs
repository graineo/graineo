//! Local certificate authority and server certificate for HTTPS in the home network.
//!
//! graineo creates its own small certificate authority once (ECDSA P-256, key never leaves the
//! data folder) and issues a server certificate for the current addresses of the computer. The
//! authority is restricted by name constraints to private address ranges and `.local` names, so
//! even a leaked key cannot vouch for public web sites. A tablet trusts the server after
//! installing the authority certificate (served at `/ca.crt`) or after accepting the warning
//! once; the SHA-256 fingerprint is shown in graineo's settings for comparison.

use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType, ExtendedKeyUsagePurpose, GeneralSubtree,
    IsCa, Issuer, KeyPair, KeyUsagePurpose, NameConstraints, SanType, PKCS_ECDSA_P256_SHA256,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CA_CERT: &str = "ca-cert.pem";
const CA_KEY: &str = "ca-key.pem";
const SERVER_CERT: &str = "server-cert.pem";
const SERVER_KEY: &str = "server-key.pem";
const INFO: &str = "certificates.json";

/// Validity of the server certificate. Apple accepts at most 825 days for certificates of
/// user-installed authorities; one year leaves room.
const SERVER_DAYS: i64 = 397;
/// Renew the server certificate when fewer days than this remain.
const RENEW_DAYS: i64 = 30;

#[derive(Serialize, Deserialize, Default, Clone)]
struct Info {
    /// common name of the authority (with a random suffix, unique per installation)
    ca_name: String,
    /// names and addresses the current server certificate covers
    server_names: Vec<String>,
    /// unix time when the server certificate expires
    server_not_after: i64,
}

/// Certificates ready for the TLS listener.
pub struct Certificates {
    pub server_config: Arc<rustls::ServerConfig>,
    /// authority certificate (DER), offered for download at `/ca.crt`
    pub ca_der: Vec<u8>,
    /// SHA-256 fingerprint of the authority certificate, `AB:CD:…`
    pub ca_fingerprint: String,
}

pub fn fingerprint(der: &[u8]) -> String {
    let d = Sha256::digest(der);
    d.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
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
    }
    fs::rename(&tmp, path)
}

fn ca_params(name: &str) -> CertificateParams {
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    dn.push(DnType::OrganizationName, "graineo (this computer only)");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    let now = time::OffsetDateTime::now_utc();
    p.not_before = now - time::Duration::days(1);
    p.not_after = now + time::Duration::days(3650);
    let v4 = |a: [u8; 4], n: u8| GeneralSubtree::IpAddress(CidrSubnet::from_v4_prefix(a, n));
    let v6 = |a: [u8; 16], n: u8| GeneralSubtree::IpAddress(CidrSubnet::from_v6_prefix(a, n));
    let mut fc = [0u8; 16];
    fc[0] = 0xfc;
    let mut fe = [0u8; 16];
    fe[0] = 0xfe;
    fe[1] = 0x80;
    let mut lo6 = [0u8; 16];
    lo6[15] = 1;
    p.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![
            v4([10, 0, 0, 0], 8),
            v4([172, 16, 0, 0], 12),
            v4([192, 168, 0, 0], 16),
            v4([169, 254, 0, 0], 16),
            v4([100, 64, 0, 0], 10),
            v4([127, 0, 0, 0], 8),
            v6(fc, 7),
            v6(fe, 10),
            v6(lo6, 128),
            GeneralSubtree::DnsName("local".into()),
            GeneralSubtree::DnsName("localhost".into()),
        ],
        excluded_subtrees: vec![],
    });
    p
}

fn random_suffix() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Names the server certificate must cover: `localhost`, the Bonjour name and all addresses.
pub fn wanted_names(addresses: &[IpAddr], host: Option<&str>) -> Vec<String> {
    let mut v = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    if let Some(h) = host {
        v.push(h.to_string());
    }
    for a in addresses {
        let s = a.to_string();
        if !v.contains(&s) {
            v.push(s);
        }
    }
    v
}

/// Loads (or creates) the authority and a server certificate covering `names`.
pub fn load_or_create(dir: &Path, names: &[String]) -> Result<Certificates, String> {
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let p = |f: &str| -> PathBuf { dir.join(f) };
    let mut info: Info = fs::read(p(INFO)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();

    // authority
    let (ca_key, ca_pem) = match (fs::read_to_string(p(CA_KEY)), fs::read_to_string(p(CA_CERT))) {
        (Ok(k), Ok(c)) if !info.ca_name.is_empty() => match KeyPair::from_pem(&k) {
            Ok(key) => (key, c),
            Err(_) => create_ca(dir, &mut info)?,
        },
        _ => create_ca(dir, &mut info)?,
    };
    let ca_der = pem_to_der(&ca_pem).ok_or("authority certificate unreadable")?;

    // server certificate
    let mut sorted: Vec<String> = names.to_vec();
    sorted.sort();
    let mut have = info.server_names.clone();
    have.sort();
    let fresh = have == sorted && info.server_not_after - now_unix() > RENEW_DAYS * 86_400;
    let (server_pem, server_key_pem) = match (fresh, fs::read_to_string(p(SERVER_CERT)), fs::read_to_string(p(SERVER_KEY))) {
        (true, Ok(c), Ok(k)) => (c, k),
        _ => {
            let issuer = Issuer::new(ca_params(&info.ca_name), ca_key);
            let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
            let mut params = CertificateParams::default();
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "graineo");
            params.distinguished_name = dn;
            params.subject_alt_names = names
                .iter()
                .filter_map(|n| match n.parse::<IpAddr>() {
                    Ok(ip) => Some(SanType::IpAddress(ip)),
                    Err(_) => n.clone().try_into().ok().map(SanType::DnsName),
                })
                .collect();
            params.is_ca = IsCa::ExplicitNoCa;
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            params.use_authority_key_identifier_extension = true;
            let now = time::OffsetDateTime::now_utc();
            params.not_before = now - time::Duration::days(1);
            params.not_after = now + time::Duration::days(SERVER_DAYS);
            let cert = params.signed_by(&key, &issuer).map_err(|e| e.to_string())?;
            let c = cert.pem();
            let k = key.serialize_pem();
            write_private(&p(SERVER_KEY), &k).map_err(|e| e.to_string())?;
            write_private(&p(SERVER_CERT), &c).map_err(|e| e.to_string())?;
            info.server_names = sorted;
            info.server_not_after = params.not_after.unix_timestamp();
            write_private(&p(INFO), &serde_json::to_string_pretty(&info).unwrap_or_default()).map_err(|e| e.to_string())?;
            (c, k)
        }
    };
    let server_der = pem_to_der(&server_pem).ok_or("server certificate unreadable")?;
    let key_der = pem_to_der(&server_key_pem).ok_or("server key unreadable")?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(
            vec![server_der.into(), ca_der.clone().into()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .map_err(|e| e.to_string())?;
    Ok(Certificates {
        server_config: Arc::new(config),
        ca_fingerprint: fingerprint(&ca_der),
        ca_der,
    })
}

fn create_ca(dir: &Path, info: &mut Info) -> Result<(KeyPair, String), String> {
    info.ca_name = format!("graineo local CA {}", random_suffix());
    info.server_names.clear();
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
    let cert = ca_params(&info.ca_name).self_signed(&key).map_err(|e| e.to_string())?;
    write_private(&dir.join(CA_KEY), &key.serialize_pem()).map_err(|e| e.to_string())?;
    write_private(&dir.join(CA_CERT), &cert.pem()).map_err(|e| e.to_string())?;
    // force a new server certificate signed by the new authority
    let _ = fs::remove_file(dir.join(SERVER_CERT));
    Ok((key, cert.pem()))
}

/// Throws away the authority and server certificate; the next start creates new ones (devices
/// must trust the new authority again).
pub fn reset(dir: &Path) {
    for f in [CA_CERT, CA_KEY, SERVER_CERT, SERVER_KEY, INFO] {
        let _ = fs::remove_file(dir.join(f));
    }
}

fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let mut body = String::new();
    let mut inside = false;
    for line in pem.lines() {
        let l = line.trim();
        if l.starts_with("-----BEGIN") {
            inside = true;
            continue;
        }
        if l.starts_with("-----END") {
            break;
        }
        if inside {
            body.push_str(l);
        }
    }
    base64_decode(&body)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_and_reissues_for_new_names() {
        let dir = std::env::temp_dir().join(format!("graineo-remote-tls-{}", random_suffix()));
        let a = load_or_create(&dir, &wanted_names(&["192.168.1.5".parse().unwrap()], Some("mac.local"))).unwrap();
        let b = load_or_create(&dir, &wanted_names(&["192.168.1.5".parse().unwrap()], Some("mac.local"))).unwrap();
        assert_eq!(a.ca_fingerprint, b.ca_fingerprint);
        let before = fs::read_to_string(dir.join(SERVER_CERT)).unwrap();
        let c = load_or_create(&dir, &wanted_names(&["10.0.0.7".parse().unwrap()], None)).unwrap();
        assert_eq!(a.ca_fingerprint, c.ca_fingerprint, "authority stays");
        assert_ne!(before, fs::read_to_string(dir.join(SERVER_CERT)).unwrap(), "server certificate renewed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join(CA_KEY)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
