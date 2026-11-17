//! The browser access with real HTTPS requests against a server on a test library.
//!
//! Only with an own data folder (never the real library). Photos: synthetic JPEGs, plus the
//! files of `GRAINEO_TEST_PHOTOS` (a folder, copied into the test folder, originals only read).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use remote::{Change, Hooks, Options, Role};

struct TestHooks {
    open: AtomicBool,
    changes: Mutex<Vec<Change>>,
}

impl Hooks for TestHooks {
    fn load_settings(&self, path: &Path, copy: Option<&str>) -> Result<develop::Settings, String> {
        let (file, _) = library::store::locate(path).ok_or("not in library")?;
        Ok(library::store::develop(file, copy).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
    }
    fn white_balance(&self, _: &Path, s: &develop::Settings) -> Option<(f32, f32)> {
        Some(if s.raw.wb_mode == "custom" { (s.raw.temperature, s.raw.tint) } else { (5200.0, 4.0) })
    }
    fn render(&self, path: &Path, s: &develop::Settings, level: u32) -> Result<image::RgbImage, String> {
        let f = library::format_from_extension(&path.to_string_lossy()).unwrap_or("JPG");
        let (mut img, _) = library::preview::load(path, f, level)?;
        // a visible change: brightness with the exposure
        let gain = 2f32.powf(s.exposure as f32);
        for p in img.pixels_mut() {
            for c in p.0.iter_mut() {
                *c = (*c as f32 * gain).min(255.0) as u8;
            }
        }
        Ok(img)
    }
    fn save_settings(&self, _: &Path, file: i64, copy: Option<&str>, before: &develop::Settings, after: &develop::Settings) -> Result<(), String> {
        let b = serde_json::to_value(before).map_err(|e| e.to_string())?;
        let a = serde_json::to_value(after).map_err(|e| e.to_string())?;
        library::store::command_step(file, copy, &b, &a, "slider", &serde_json::json!({ "k": "remote" }), 1, false);
        library::store::set_develop(file, copy, Some(a.to_string()));
        Ok(())
    }
    fn is_open(&self, _: i64, _: Option<&str>) -> bool {
        self.open.load(Ordering::SeqCst)
    }
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
}

struct Client {
    addr: SocketAddr,
    config: Arc<rustls::ClientConfig>,
}

struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

impl Client {
    fn new(addr: SocketAddr, ca: Vec<u8>) -> Client {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls_pki_types::CertificateDer::from(ca)).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
        Client { addr, config: Arc::new(config) }
    }

    /// Sends raw request text (the head is completed with Host and Connection: close unless given).
    fn raw(&self, head: &str, body: &[u8]) -> Answer {
        let name = rustls_pki_types::ServerName::try_from("127.0.0.1").unwrap();
        let conn = rustls::ClientConnection::new(self.config.clone(), name).unwrap();
        let sock = TcpStream::connect(self.addr).unwrap();
        sock.set_read_timeout(Some(std::time::Duration::from_secs(30))).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, sock);
        let mut text = head.to_string();
        if !text.to_ascii_lowercase().contains("\r\nhost:") {
            text.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.addr.port()));
        }
        text.push_str("Connection: close\r\n");
        if !body.is_empty() {
            text.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        text.push_str("\r\n");
        tls.write_all(text.as_bytes()).unwrap();
        tls.write_all(body).unwrap();
        tls.flush().unwrap();
        let mut out = Vec::new();
        let _ = tls.read_to_end(&mut out);
        parse(&out)
    }

    /// Opens a TLS connection and sends exactly these bytes (nothing added).
    fn open(&self, bytes: &[u8]) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
        let name = rustls_pki_types::ServerName::try_from("127.0.0.1").unwrap();
        let conn = rustls::ClientConnection::new(self.config.clone(), name).unwrap();
        let sock = TcpStream::connect(self.addr).unwrap();
        sock.set_read_timeout(Some(std::time::Duration::from_secs(30))).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, sock);
        tls.write_all(bytes).unwrap();
        tls.flush().unwrap();
        tls
    }

    /// Sends exactly these bytes and reads until the server closes.
    fn exact(&self, bytes: &[u8]) -> Answer {
        let mut tls = self.open(bytes);
        let mut out = Vec::new();
        let _ = tls.read_to_end(&mut out);
        parse(&out)
    }

    fn get(&self, path: &str, cookie: Option<&str>) -> Answer {
        let c = cookie.map(|c| format!("Cookie: __Host-graineo_device={c}\r\n")).unwrap_or_default();
        self.raw(&format!("GET {path} HTTP/1.1\r\n{c}"), b"")
    }

    fn post(&self, path: &str, cookie: Option<&str>, csrf: Option<&str>, origin: Option<&str>, body: &str) -> Answer {
        let mut h = format!("POST {path} HTTP/1.1\r\nContent-Type: application/json\r\n");
        if let Some(c) = cookie {
            h.push_str(&format!("Cookie: __Host-graineo_device={c}\r\n"));
        }
        if let Some(c) = csrf {
            h.push_str(&format!("X-Graineo-CSRF: {c}\r\n"));
        }
        if let Some(o) = origin {
            h.push_str(&format!("Origin: {o}\r\n"));
        }
        self.raw(&h, body.as_bytes())
    }
}

fn parse(b: &[u8]) -> Answer {
    let end = b.windows(4).position(|w| w == b"\r\n\r\n").expect("answer head");
    let head = String::from_utf8_lossy(&b[..end]).to_string();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let mut body = b[end + 4..].to_vec();
    if headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("Transfer-Encoding") && v.contains("chunked")) {
        body = unchunk(&body);
    }
    Answer { status, headers, body }
}

fn unchunk(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = b[i..].windows(2).position(|w| w == b"\r\n") {
        let n = usize::from_str_radix(std::str::from_utf8(&b[i..i + p]).unwrap().trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.extend_from_slice(&b[i + p + 2..i + p + 2 + n]);
        i += p + 2 + n + 2;
    }
    out
}

fn cookie_of(a: &Answer) -> String {
    let c = a.header("Set-Cookie").expect("cookie");
    assert!(c.contains("HttpOnly") && c.contains("Secure") && c.contains("SameSite=Strict"), "{c}");
    c.split(';').next().unwrap().split_once('=').unwrap().1.to_string()
}

fn make_photos(dir: &Path) -> usize {
    std::fs::create_dir_all(dir).unwrap();
    let mut n = 0;
    for i in 0..3u8 {
        let img = image::RgbImage::from_fn(640, 427, |x, y| image::Rgb([(x / 3) as u8, (y / 2) as u8, 60 * i]));
        img.save(dir.join(format!("synthetic-{i}.jpg"))).unwrap();
        n += 1;
    }
    if let Some(src) = std::env::var_os("GRAINEO_TEST_PHOTOS").map(PathBuf::from) {
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            let p = e.path();
            if p.is_file() && library::format_from_extension(&p.to_string_lossy()).is_some() {
                std::fs::copy(&p, dir.join(p.file_name().unwrap())).unwrap();
                n += 1;
            }
        }
    }
    n
}

#[test]
fn browser_access_end_to_end() {
    // own data folder: never the real library
    let base = std::env::temp_dir().join(format!("graineo-remote-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("GRAINEO_DATA_DIR", base.join("data"));
    std::env::set_var("GRAINEO_LIBRARY_NO_SYNC", "1");
    std::env::set_var("GRAINEO_NO_PHOTOKIT", "1");
    std::env::set_var("GRAINEO_NO_XMP", "1");
    std::env::set_var("GRAINEO_PREVIEW_DIR", base.join("previews"));
    assert!(library::db_path().starts_with(&base), "test library in the test folder");
    let photos = base.join("photos");
    let count = make_photos(&photos);
    {
        let db = library::db::Db::open(&library::db_path()).unwrap();
        library::reconcile::add_root(&db, &photos, &|_| {}).unwrap();
    }

    let hooks = Arc::new(TestHooks { open: AtomicBool::new(false), changes: Mutex::new(Vec::new()) });
    let log = base.join("logs").join("remote-access.log");
    let opts = Options { bind: SocketAddr::from(([127, 0, 0, 1], 0)), allow_outside: false, device_days: 30, dir: base.join("data").join("remote"), log_file: Some(log.clone()) };
    let server = remote::start(opts, hooks.clone()).unwrap();
    let addr = SocketAddr::from(([127, 0, 0, 1], server.port));
    let c = Client::new(addr, server.ca_der());
    let origin = format!("https://127.0.0.1:{}", server.port);
    // listening on IPv4 only: no IPv6 addresses are offered
    assert!(server.urls.iter().all(|u| !u.contains('[')), "{:?}", server.urls);
    let o = Some(origin.as_str());

    // page without sign-in, with security headers; nothing private in it
    let a = c.get("/", None);
    assert_eq!(a.status, 200);
    assert!(a.header("Content-Security-Policy").unwrap().contains("default-src 'none'"));
    assert_eq!(a.header("X-Frame-Options"), Some("DENY"));
    assert!(a.header("Access-Control-Allow-Origin").is_none(), "no CORS");
    assert_eq!(c.get("/ca.crt", None).status, 200);

    // the internal server refuses connections that do not come through TLS
    let mut direct = TcpStream::connect(server.internal_addr().unwrap()).unwrap();
    direct.write_all(b"GET /api/session HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = Vec::new();
    let _ = direct.read_to_end(&mut out);
    assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 403"), "{}", String::from_utf8_lossy(&out));

    // nothing without pairing
    for p in ["/api/session", "/api/sources", "/api/images", "/api/image/1", "/api/image/1/thumb", "/api/image/1/large"] {
        assert_eq!(c.get(p, None).status, 401, "{p}");
        assert_eq!(c.get(p, Some(&"ab".repeat(32))).status, 401, "{p} with a made-up token");
    }

    // pairing: only with an active code, the right origin and the right code
    assert_eq!(c.post("/api/pair", None, None, o, r#"{"code":"ABCD-EFGH"}"#).json()["error"], "no_code");
    let (code, _) = server.start_pairing(Role::Write);
    assert_eq!(c.post("/api/pair", None, None, None, &format!(r#"{{"code":"{code}"}}"#)).status, 403, "no origin");
    assert_eq!(c.post("/api/pair", None, None, Some("https://evil.example"), &format!(r#"{{"code":"{code}"}}"#)).status, 403, "foreign origin");
    assert_eq!(c.post("/api/pair", None, None, o, r#"{"code":"WXYZ-2345"}"#).json()["error"], "wrong_code");
    let a = c.post("/api/pair", None, None, o, &format!(r#"{{"code":"{code}","name":"Test-iPad"}}"#));
    assert_eq!(a.status, 200);
    let token = cookie_of(&a);
    let csrf = a.json()["csrf"].as_str().unwrap().to_string();
    assert!(!String::from_utf8_lossy(&a.body).contains(&token), "token only in the cookie");
    assert_eq!(c.post("/api/pair", None, None, o, &format!(r#"{{"code":"{code}"}}"#)).json()["error"], "no_code", "code used up");
    let t = Some(token.as_str());

    // reading
    assert_eq!(c.get("/api/session", t).json()["device"]["name"], "Test-iPad");
    assert_eq!(c.get("/api/sources", t).status, 200);
    let list = c.get("/api/images?limit=500", t).json();
    assert_eq!(list["total"].as_u64().unwrap() as usize, count, "{list}");
    let id = list["items"][0]["id"].as_i64().unwrap();
    let jpeg_id = list["items"].as_array().unwrap().iter().find(|i| i["name"].as_str().unwrap().starts_with("synthetic")).unwrap()["id"].as_i64().unwrap();
    let a = c.get(&format!("/api/image/{jpeg_id}/thumb"), t);
    assert_eq!(a.status, 200);
    assert_eq!(a.header("Content-Type"), Some("image/jpeg"));
    assert!(a.body.starts_with(&[0xff, 0xd8]));
    assert_eq!(c.get(&format!("/api/image/{id}/large"), t).status, 200);
    assert_eq!(c.get(&format!("/api/image/{id}"), t).status, 200);
    assert_eq!(c.get("/api/images?src=folder:abc", t).status, 400);

    // paths never reach files: only listed assets and numeric ids
    for p in [
        "/../../../../etc/passwd",
        "/%2e%2e/%2e%2e/etc/passwd",
        "/api/image/../../../etc/passwd",
        "/api/image/%2e%2e%2f%2e%2e%2fetc%2fpasswd/thumb",
        "/api/image/1%2f..%2f..%2fCargo.toml/thumb",
        "/api/image/-1/thumb",
        "/api/image/99999999/thumb",
        "/api/image/1.jpg/thumb",
        "/fonts/../../Cargo.toml",
        "/fonts/..%2f..%2fCargo.toml",
        "/api/images/../../library.sqlite",
        "/library.sqlite",
        "/devices.json",
        "/ca-key.pem",
    ] {
        let a = c.get(p, t);
        assert!(a.status == 404 || a.status == 400, "{p}: {}", a.status);
        assert!(!String::from_utf8_lossy(&a.body).contains("root:"), "{p}");
    }

    // DNS rebinding: a foreign host name gets nothing
    let a = c.raw(&format!("GET /api/session HTTP/1.1\r\nHost: evil.example:{}\r\nCookie: __Host-graineo_device={token}\r\n", server.port), b"");
    assert_eq!(a.status, 421);

    // writes need CSRF header and origin
    let rating = format!("/api/image/{id}/rating");
    assert_eq!(c.post(&rating, t, None, o, r#"{"stars":4}"#).json()["error"], "csrf");
    assert_eq!(c.post(&rating, t, Some("0".repeat(32).as_str()), o, r#"{"stars":4}"#).json()["error"], "csrf");
    assert_eq!(c.post(&rating, t, Some(&csrf), Some("https://evil.example"), r#"{"stars":4}"#).json()["error"], "origin");
    assert_eq!(c.post(&rating, t, Some(&csrf), None, r#"{"stars":4}"#).json()["error"], "origin");
    assert_eq!(c.post(&rating, t, Some(&csrf), o, r#"{"stars":9}"#).status, 400);
    assert_eq!(c.post(&rating, t, Some(&csrf), o, r#"{"path":"/etc/passwd"}"#).status, 400);
    assert_eq!(c.post(&rating, t, Some(&csrf), o, "not json").status, 400);
    let big = format!(r#"{{"stars":1,"x":"{}"}}"#, "a".repeat(70 * 1024));
    assert_eq!(c.post(&rating, t, Some(&csrf), o, &big).status, 413);
    let a = c.raw(&format!("POST {rating} HTTP/1.1\r\nContent-Type: text/plain\r\nCookie: __Host-graineo_device={token}\r\nX-Graineo-CSRF: {csrf}\r\nOrigin: {origin}\r\n"), br#"{"stars":4}"#);
    assert_eq!(a.status, 415);
    let a = c.post(&rating, t, Some(&csrf), o, r#"{"stars":4,"flag":1,"label":3}"#);
    assert_eq!(a.status, 200, "{}", String::from_utf8_lossy(&a.body));
    assert!(library::store::flush(std::time::Duration::from_secs(10)));
    {
        let db = library::db::Db::read_only(&library::db_path()).unwrap();
        let z = db.rows().unwrap().into_iter().find(|z| z.image == id).unwrap();
        assert_eq!((z.stars, z.flag, z.label), (4, 1, 3), "in the library through library::store");
    }
    assert!(hooks.changes.lock().unwrap().iter().any(|c| matches!(c, Change::Rating { image, stars: 4, .. } if *image == id)), "app told");
    // two changes at once (stars and label from two quick taps) both stay
    std::thread::scope(|sc| {
        sc.spawn(|| c.post(&rating, t, Some(&csrf), o, r#"{"stars":2}"#));
        sc.spawn(|| c.post(&rating, t, Some(&csrf), o, r#"{"label":5}"#));
    });
    assert_eq!(c.get(&format!("/api/image/{id}"), t).json()["stars"], 2);
    assert_eq!(c.get(&format!("/api/image/{id}"), t).json()["label"], 5);

    // develop: values, preview, save with version check, history
    let a = c.get(&format!("/api/image/{jpeg_id}/develop"), t);
    assert_eq!(a.status, 200, "{}", String::from_utf8_lossy(&a.body));
    let dv = a.json();
    assert_eq!(dv["kind"], "finished");
    let version = dv["version"].as_str().unwrap().to_string();
    let a = c.post(&format!("/api/image/{jpeg_id}/render"), t, Some(&csrf), o, r#"{"values":{"exposure":1.0}}"#);
    assert_eq!(a.status, 200);
    assert!(a.body.starts_with(&[0xff, 0xd8]));
    assert_eq!(c.post(&format!("/api/image/{jpeg_id}/render"), t, Some(&csrf), o, r#"{"values":{"exposure":42}}"#).status, 400, "out of range");
    assert_eq!(c.post(&format!("/api/image/{jpeg_id}/render"), t, Some(&csrf), o, r#"{"values":{"crop":[0,0,1,1]}}"#).status, 400, "unknown value");
    let save = format!("/api/image/{jpeg_id}/develop");
    assert_eq!(c.post(&save, t, Some(&csrf), o, r#"{"values":{"exposure":0.5},"version":"0000"}"#).json()["error"], "changed_elsewhere");
    hooks.open.store(true, Ordering::SeqCst);
    assert_eq!(c.post(&save, t, Some(&csrf), o, &format!(r#"{{"values":{{"exposure":0.5}},"version":"{version}"}}"#)).json()["error"], "open_in_app");
    hooks.open.store(false, Ordering::SeqCst);
    let a = c.post(&save, t, Some(&csrf), o, &format!(r#"{{"values":{{"exposure":0.5,"contrast":10}},"version":"{version}"}}"#));
    assert_eq!(a.status, 200, "{}", String::from_utf8_lossy(&a.body));
    assert_eq!(a.json()["saved"], true);
    assert!(library::store::flush(std::time::Duration::from_secs(10)));
    let snap = remote::catalog::snapshot().unwrap();
    let file = snap.row(jpeg_id).unwrap().file;
    let stored: develop::Settings = serde_json::from_str(&library::store::develop(file, None).unwrap()).unwrap();
    assert_eq!(stored.exposure, 0.5);
    assert!((stored.contrast - 0.1).abs() < 1e-9);
    let (steps, _) = library::store::load_history(file, None).unwrap();
    assert!(steps.len() >= 2, "history step recorded: {}", steps.len());
    // the app gets the new development for its grid
    let mut told = false;
    for _ in 0..100 {
        if hooks.changes.lock().unwrap().iter().any(|c| matches!(c, Change::Develop { image, preview: Some(_), .. } if *image == jpeg_id)) {
            told = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(told, "develop change with preview");
    // the old version is no longer accepted
    assert_eq!(c.post(&save, t, Some(&csrf), o, &format!(r#"{{"values":{{"exposure":1}},"version":"{version}"}}"#)).json()["error"], "changed_elsewhere");

    // a device paired for viewing only cannot write
    let (code2, _) = server.start_pairing(Role::Read);
    let a = c.post("/api/pair", None, None, o, &format!(r#"{{"code":"{code2}","name":"Viewer"}}"#));
    let token2 = cookie_of(&a);
    let csrf2 = a.json()["csrf"].as_str().unwrap().to_string();
    assert_eq!(c.post(&rating, Some(&token2), Some(&csrf2), o, r#"{"stars":1}"#).json()["error"], "read_only");
    assert_eq!(c.get(&format!("/api/image/{jpeg_id}/develop"), Some(&token2)).json()["error"], "read_only");
    assert_eq!(c.post(&save, Some(&token2), Some(&csrf2), o, r#"{"values":{"exposure":1}}"#).json()["error"], "read_only");
    // the CSRF value of one device does not work for another
    assert_eq!(c.post(&rating, t, Some(&csrf2), o, r#"{"stars":1}"#).json()["error"], "csrf");

    // slow clients (a request head with a large announced body, then nothing) do not block the server: requests are
    // only passed on when complete
    let mut slow = Vec::new();
    for _ in 0..6 {
        let name = rustls_pki_types::ServerName::try_from("127.0.0.1").unwrap();
        let conn = rustls::ClientConnection::new(c.config.clone(), name).unwrap();
        let sock = TcpStream::connect(c.addr).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, sock);
        tls.write_all(format!("POST /api/pair HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: 60000\r\n\r\n{{", server.port).as_bytes()).unwrap();
        tls.flush().unwrap();
        slow.push(tls);
    }
    let t0 = std::time::Instant::now();
    assert_eq!(c.get("/api/session", t).status, 200);
    assert!(t0.elapsed() < std::time::Duration::from_secs(3), "server answers while slow clients wait");
    drop(slow);
    // framing limits of the front
    let a = c.raw("POST /api/pair HTTP/1.1\r\nTransfer-Encoding: chunked\r\n", b"0\r\n\r\n");
    assert_eq!(a.status, 400);
    // request smuggling: everything the front and tiny_http could read differently is refused before it is passed on
    let port = server.port;
    let pair_head = format!("POST /api/pair HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\n");
    for (extra, status) in [
        ("Content-Length: 2000\r\nContent-Length: 0\r\n", 400),
        ("Content-Length: 0\r\nContent-Length: 2000\r\n", 400),
        ("Connection: upgrade\r\nContent-Length: 0\r\n", 400),
        ("Connection: keep-alive, Upgrade\r\nUpgrade: h2c\r\n", 400),
        ("Expect: 100-continue\r\nContent-Length: 2\r\n", 417),
        ("Expect: 200-ok\r\n", 417),
        ("X-Bare: a\nContent-Length: 2000\r\n", 400),
        ("Content-Length:  +2\r\n", 400),
        ("Content-Length: 2\r\n Content-Length: 2000\r\n", 400),
        ("Transfer-Encoding: identity\r\nContent-Length: 2\r\n", 400),
        ("no-colon\r\n", 400),
    ] {
        let a = c.exact(format!("{pair_head}{extra}\r\n{{}}").as_bytes());
        assert_eq!(a.status, status, "{extra:?}");
    }
    // the attack from the review: four connections with two lengths each must not tie up the four workers
    let t0 = std::time::Instant::now();
    let held: Vec<_> = (0..4).map(|_| c.open(format!("{pair_head}Content-Length: 2000\r\nContent-Length: 0\r\n\r\n").as_bytes())).collect();
    for _ in 0..4 {
        assert_eq!(c.get("/api/session", t).status, 200);
    }
    assert!(t0.elapsed() < std::time::Duration::from_secs(3), "workers stay free: {:?}", t0.elapsed());
    drop(held);
    // pipelined: a complete request, then a smuggled one on the same connection – the second is refused, not run
    let a = c.exact(format!("GET /api/session HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: __Host-graineo_device={token}\r\n\r\nGET /api/session HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 5\r\nContent-Length: 0\r\n\r\n").as_bytes());
    assert_eq!(a.status, 200);
    // a broken pairing body is a failed attempt (counted for the block below) and frees its worker at once
    let a = c.post("/api/pair", None, None, o, "{not json");
    assert_eq!(a.status, 400);
    let a = c.raw(&format!("GET / HTTP/1.1\r\nX-Big: {}\r\n", "a".repeat(20_000)), b"");
    assert_eq!(a.status, 431);

    // revoking
    server.revoke(&server.devices().iter().find(|d| d.name == "Viewer").unwrap().id);
    assert_eq!(c.get("/api/session", Some(&token2)).status, 401);
    assert_eq!(c.post("/api/logout", t, Some(&csrf), o, "{}").status, 200);
    assert_eq!(c.get("/api/session", t).status, 401, "signed out");
    assert!(server.devices().is_empty());

    // access log: events without tokens, codes or query texts
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("paired") && text.contains("pairing failed") && text.contains("csrf"), "{text}");
    for secret in [&token, &token2, &csrf, &code, &code2] {
        assert!(!text.contains(secret.as_str()), "secret in the log");
    }
    assert!(!text.contains("passwd"), "no request contents in the log");
    // stored device file holds no token
    let devfile = std::fs::read_to_string(base.join("data").join("remote").join("devices.json")).unwrap();
    assert!(!devfile.contains(&token));

    // pairing attempts are limited: after 10 failures the address is blocked, even with the right code
    let (code3, _) = server.start_pairing(Role::Write);
    for _ in 0..12 {
        let _ = c.post("/api/pair", None, None, o, r#"{"code":"AAAA-AAAA"}"#);
        let _ = server.start_pairing(Role::Write);
    }
    let (code3b, _) = server.start_pairing(Role::Write);
    assert_ne!(code3, code3b);
    assert_eq!(c.post("/api/pair", None, None, o, &format!(r#"{{"code":"{code3b}"}}"#)).json()["error"], "blocked");

    // request rate: a flood is cut off
    let mut limited = false;
    for _ in 0..3000 {
        if c.get("/app.css", None).status == 429 {
            limited = true;
            break;
        }
    }
    assert!(limited, "rate limit");

    // switching off does not wait for the workers (the app calls it from its UI thread), and a restart gets the port
    let t0 = std::time::Instant::now();
    server.stop();
    assert!(t0.elapsed() < std::time::Duration::from_millis(1500), "stop took {:?}", t0.elapsed());
    let opts = Options { bind: addr, allow_outside: false, device_days: 30, dir: base.join("data").join("remote"), log_file: None };
    let again = remote::start(opts, hooks.clone()).expect("same port again at once");
    assert_eq!(Client::new(addr, again.ca_der()).get("/", None).status, 200);
    again.stop();
    let _ = std::fs::remove_dir_all(&base);
}
