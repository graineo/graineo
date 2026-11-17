//! Request handling: checks every request (origin of the connection, rate, host, sign-in,
//! CSRF, role) before it reaches the library.

use std::io::Read;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response};

use crate::access_log::AccessLog;
use crate::catalog::{self, PreviewError, Size};
use crate::develop_edit::{self, Kind};
use crate::devices::{Device, Devices, PairError, Role};
use crate::limits::{Buckets, Failures, PerKey, Slots};
use crate::{net, proxy, tls, Change, Hooks, Options};

/// Largest request body (JSON of a rating or of slider values).
const MAX_BODY: usize = 64 * 1024;
/// Name of the cookie with the device token.
/// `__Host-`: the browser sends it only to exactly this origin (not to other ports or subdomains)
const COOKIE: &str = "__Host-graineo_device";
/// Long edge of the develop preview in the browser.
const RENDER_LEVEL: u32 = 1600;

pub(crate) struct State {
    hooks: Arc<dyn Hooks>,
    pub devices: Devices,
    log: AccessLog,
    peers: Arc<proxy::Peers>,
    requests: Buckets,
    sign_in: Failures,
    previews: Slots,
    renders: Slots,
    /// pairing requests (before any sign-in) being handled: at most 2 of the 4 workers, one per address
    pairing: Slots,
    pairing_per_peer: PerKey,
    /// developments for the app's grid after a save from the browser: one thread, a short queue
    after_save: Mutex<Option<std::sync::mpsc::SyncSender<AfterSave>>>,
    allow_outside: Arc<AtomicBool>,
    device_days: i64,
    ca_der: Vec<u8>,
    host_names: Vec<String>,
    /// writes per address (ratings, saves): one per second on average, bursts of up to 30 (quick taps through a series)
    writes: Buckets,
    rate_logged: Buckets,
}

/// The running server; dropping it does not stop it, call [`Running::stop`].
pub struct Running {
    state: Arc<State>,
    front: proxy::Front,
    http: Arc<tiny_http::Server>,
    stop: Arc<AtomicBool>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub port: u16,
    pub fingerprint: String,
    pub urls: Vec<String>,
    pub dir: PathBuf,
}

impl Running {
    pub fn devices(&self) -> Vec<Device> {
        self.state.devices.list()
    }

    pub fn start_pairing(&self, role: Role) -> (String, i64) {
        self.state.devices.start_pairing(role)
    }

    pub fn pairing(&self) -> Option<(String, i64)> {
        self.state.devices.pairing()
    }

    pub fn cancel_pairing(&self) {
        self.state.devices.cancel_pairing()
    }

    pub fn revoke(&self, id: &str) {
        self.state.devices.revoke(id);
    }

    pub fn revoke_all(&self) {
        self.state.devices.revoke_all();
    }

    pub fn set_role(&self, id: &str, role: Role) {
        self.state.devices.set_role(id, role);
    }

    pub fn set_allow_outside(&self, on: bool) {
        self.state.allow_outside.store(on, Ordering::SeqCst);
    }

    /// Previews or developments the server is computing right now (for the activity display of the app).
    pub fn busy(&self) -> usize {
        self.state.previews.used() + self.state.renders.used()
    }

    pub fn log_tail(&self, n: usize) -> Vec<String> {
        self.state.log.tail(n)
    }

    /// Certificate of the local authority (DER), for clients that pin it (tests).
    pub fn ca_der(&self) -> Vec<u8> {
        self.state.ca_der.clone()
    }

    /// Address of the internal plain HTTP server (only for tests that check it refuses direct connections).
    #[doc(hidden)]
    pub fn internal_addr(&self) -> Option<SocketAddr> {
        self.http.server_addr().to_ip()
    }

    pub fn log_path(&self) -> Option<PathBuf> {
        self.state.log.path().cloned()
    }

    /// Stops accepting connections and ends the worker threads (open connections end on their own).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.front.shutdown();
        for _ in 0..8 {
            self.http.unblock();
        }
        let handles: Vec<_> = self.workers.lock().unwrap().drain(..).collect();
        // the workers end within half a second, or after the preview or development they are computing (seconds):
        // never wait for that on the caller's thread (the app calls this from its UI thread)
        let join = move || {
            for h in handles {
                let _ = h.join();
            }
        };
        let _ = std::thread::Builder::new().name("remote-stop".into()).spawn(join);
    }
}

pub(crate) fn start(opts: Options, hooks: Arc<dyn Hooks>, stop: Arc<AtomicBool>) -> Result<Running, String> {
    let addresses = net::local_addresses();
    let host = net::local_host_name();
    let names = tls::wanted_names(&addresses, host.as_deref());
    let certs = tls::load_or_create(&opts.dir, &names)?;
    let listener = TcpListener::bind(opts.bind).map_err(|e| format!("{}: {e}", opts.bind))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let http = Arc::new(tiny_http::Server::http(SocketAddr::from(([127, 0, 0, 1], 0))).map_err(|e| e.to_string())?);
    let internal = http.server_addr().to_ip().ok_or("internal address")?;
    let allow_outside = Arc::new(AtomicBool::new(opts.allow_outside));
    let front = proxy::start(listener, internal, certs.server_config.clone(), allow_outside.clone()).map_err(|e| e.to_string())?;
    let mut host_names: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    // IPv6 addresses only when the server listens there (the standard bind 0.0.0.0 is IPv4 only)
    let v6 = opts.bind.is_ipv6();
    if v6 {
        for a in &addresses {
            if let IpAddr::V6(v6) = a {
                host_names.push(format!("[{v6}]"));
            }
        }
        host_names.push("[::1]".into());
    }
    let state = Arc::new(State {
        hooks,
        devices: Devices::load(&opts.dir),
        log: AccessLog::new(opts.log_file.clone()),
        peers: front.peers.clone(),
        // a grid of thumbnails asks for many images at once
        requests: Buckets::new(40.0, 400.0),
        sign_in: Failures::new(10, Duration::from_secs(600), Duration::from_secs(900)),
        previews: Slots::new(2),
        renders: Slots::new(1),
        pairing: Slots::new(2),
        pairing_per_peer: PerKey::new(1),
        after_save: Mutex::new(None),
        allow_outside,
        device_days: opts.device_days,
        ca_der: certs.ca_der.clone(),
        host_names,
        writes: Buckets::new(1.0, 30.0),
        rate_logged: Buckets::new(1.0 / 60.0, 1.0),
    });
    let mut workers = Vec::new();
    for i in 0..4 {
        let (st, srv, s) = (state.clone(), http.clone(), stop.clone());
        let h = std::thread::Builder::new()
            .name(format!("remote-http-{i}"))
            .spawn(move || {
                while !s.load(Ordering::SeqCst) {
                    match srv.recv_timeout(Duration::from_millis(500)) {
                        Ok(Some(rq)) => {
                            library::platform::set_priority(library::platform::Priority::Utility);
                            handle(&st, rq);
                        }
                        Ok(None) => {}
                        Err(_) => {}
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        workers.push(h);
    }
    let urls = addresses.iter().filter(|a| v6 || a.is_ipv4()).map(|a| net::url_for(*a, port)).chain(host.iter().map(|h| format!("https://{h}:{port}"))).collect();
    Ok(Running { state, front, http, stop, workers: Mutex::new(workers), port, fingerprint: certs.ca_fingerprint, urls, dir: opts.dir })
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

/// Headers on every answer.
fn secure(mut r: Response<std::io::Cursor<Vec<u8>>>, cache: bool) -> Response<std::io::Cursor<Vec<u8>>> {
    for (n, v) in [
        ("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' blob:; font-src 'self'; connect-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        ("Cross-Origin-Opener-Policy", "same-origin"),
        ("Cross-Origin-Resource-Policy", "same-origin"),
        ("Permissions-Policy", "camera=(), microphone=(), geolocation=(), interest-cohort=()"),
        ("Strict-Transport-Security", "max-age=31536000"),
    ] {
        r = r.with_header(header(n, v));
    }
    r.with_header(header("Cache-Control", if cache { "private, max-age=3600" } else { "no-store" }))
}

fn json_response(status: u16, v: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    secure(Response::from_data(v.to_string().into_bytes()).with_status_code(status).with_header(header("Content-Type", "application/json; charset=utf-8")), false)
}

fn error(status: u16, code: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(status, &json!({ "error": code }))
}

fn get_header<'a>(rq: &'a Request, name: &str) -> Option<&'a str> {
    rq.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}

fn cookie_token(rq: &Request) -> Option<String> {
    let c = get_header(rq, "Cookie")?;
    c.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == COOKIE).map(|(_, v)| v.trim().to_string())
}

/// Host header without port, lower case; `[v6]` keeps its brackets.
fn host_of(rq: &Request) -> Option<String> {
    let h = get_header(rq, "Host")?.trim().to_ascii_lowercase();
    let name = if h.starts_with('[') { h.split(']').next().map(|s| format!("{s}]"))? } else { h.split(':').next()?.to_string() };
    Some(name)
}

/// Only our own addresses and names are accepted as Host (defence against DNS rebinding: a
/// foreign web page that resolves its name to this computer gets nothing).
fn host_ok(st: &State, host: &str) -> bool {
    if st.host_names.iter().any(|n| n == host) {
        return true;
    }
    // a new address of this computer (DHCP, VPN) that is in the home network
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<IpAddr>() {
        Ok(ip) => net::is_local_network(ip) || st.allow_outside.load(Ordering::SeqCst),
        Err(_) => false,
    }
}

fn origin_ok(rq: &Request, host_header: &str) -> bool {
    // the browser sends Origin with every POST from a page; it must be this server
    match get_header(rq, "Origin") {
        Some(o) => o.eq_ignore_ascii_case(&format!("https://{host_header}")),
        None => false,
    }
}

fn read_body(rq: &mut Request) -> Result<Value, u16> {
    let len = rq.body_length().ok_or(411u16)?;
    if len > MAX_BODY {
        return Err(413);
    }
    if !get_header(rq, "Content-Type").is_some_and(|c| c.to_ascii_lowercase().starts_with("application/json")) {
        return Err(415);
    }
    let mut buf = Vec::with_capacity(len);
    rq.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut buf).map_err(|_| 400u16)?;
    if buf.len() > MAX_BODY {
        return Err(413);
    }
    serde_json::from_slice(&buf).map_err(|_| 400)
}

fn query_params(url: &str) -> Vec<(String, String)> {
    let Some((_, q)) = url.split_once('?') else { return Vec::new() };
    q.split('&')
        .filter_map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            Some((percent_decode(k)?, percent_decode(v)?))
        })
        .collect()
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 2;
            }
            b'%' => return None,
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Image id from a path segment: digits only, nothing else.
fn parse_id(s: &str) -> Option<i64> {
    if s.is_empty() || s.len() > 18 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn handle(st: &State, rq: Request) {
    let Some(peer) = rq.remote_addr().and_then(|a| st.peers.lookup(*a)) else {
        // not through the TLS front (another program on this computer): refuse
        let _ = rq.respond(Response::empty(403));
        return;
    };
    // "also from outside" switched off while a connection from outside is still open: it gets nothing more
    if !net::is_local_network(peer) && !st.allow_outside.load(Ordering::SeqCst) {
        let _ = rq.respond(Response::empty(403));
        return;
    }
    let method = rq.method().clone();
    let url = rq.url().to_string();
    if url.len() > 2048 {
        let _ = rq.respond(error(414, "uri_too_long"));
        return;
    }
    let key = net::limit_key(peer);
    if !st.requests.take(key, 1.0) {
        // logged at most once a minute per address (a flood must not turn into a flood of log lines)
        if st.rate_logged.take(key, 1.0) {
            st.log.write(peer, None, method.as_str(), &url, 429, "rate");
        }
        let _ = rq.respond(error(429, "rate_limited").with_header(header("Retry-After", "2")));
        return;
    }
    let Some(host) = host_of(&rq) else {
        let _ = rq.respond(error(400, "host"));
        return;
    };
    if !host_ok(st, &host) {
        st.log.write(peer, None, method.as_str(), &url, 421, "foreign host");
        let _ = rq.respond(error(421, "host"));
        return;
    }
    let host_header = get_header(&rq, "Host").unwrap_or("").trim().to_string();
    let path = url.split(['?', '#']).next().unwrap_or("").to_string();

    // static files of the page: no sign-in needed (they contain nothing private)
    if method == Method::Get || method == Method::Head {
        if let Some((body, ctype)) = crate::web::asset(&path) {
            let r = Response::from_data(body.to_vec()).with_header(header("Content-Type", ctype));
            let _ = rq.respond(secure(r, path != "/" && path != "/index.html"));
            return;
        }
        if path == "/ca.crt" {
            let r = Response::from_data(st.ca_der.clone())
                .with_header(header("Content-Type", "application/x-x509-ca-cert"))
                .with_header(header("Content-Disposition", "attachment; filename=\"graineo-local-ca.crt\""));
            let _ = rq.respond(secure(r, false));
            return;
        }
    }
    if !path.starts_with("/api/") {
        let _ = rq.respond(error(404, "not_found"));
        return;
    }
    let is_write = method != Method::Get && method != Method::Head;
    if is_write && !origin_ok(&rq, &host_header) {
        st.log.write(peer, None, method.as_str(), &url, 403, "origin");
        let _ = rq.respond(error(403, "origin"));
        return;
    }
    if path == "/api/pair" {
        pair(st, rq, peer, &method, &url);
        return;
    }
    let device = cookie_token(&rq).and_then(|t| st.devices.check(&t));
    let Some(device) = device else {
        // (an invalid cookie is not counted as a failed sign-in: tokens have 256 random bits, and a revoked
        // device's page sends its old cookie with several requests at once before the answer clears it)
        if cookie_token(&rq).is_some() {
            st.log.write(peer, None, method.as_str(), &url, 401, "invalid token");
        }
        let _ = rq.respond(error(401, "not_paired").with_header(header("Set-Cookie", &format!("{COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict"))));
        return;
    };
    if is_write {
        let csrf = get_header(&rq, "X-Graineo-CSRF").unwrap_or("");
        if !crate::devices::same(csrf, &device.csrf) {
            st.log.write(peer, Some(&device.id), method.as_str(), &url, 403, "csrf");
            let _ = rq.respond(error(403, "csrf"));
            return;
        }
    }
    let status = route(st, rq, peer, &device, &method, &path, &url);
    let quiet = status == 200 && method == Method::Get && (path.ends_with("/thumb") || path.ends_with("/large") || path == "/api/images" || path.ends_with("/develop"));
    if !quiet {
        // unknown routes are logged without their path (it is whatever the client sent)
        let logged = if status == 404 { "/api/?" } else { url.as_str() };
        st.log.write(peer, Some(&device.id), method.as_str(), logged, status, "");
    }
}

fn pair(st: &State, mut rq: Request, peer: IpAddr, method: &Method, url: &str) {
    if *method != Method::Post {
        let _ = rq.respond(error(405, "method"));
        return;
    }
    let key = net::limit_key(peer);
    if st.sign_in.blocked(key) {
        st.log.write(peer, None, "POST", url, 429, "pairing blocked");
        let _ = rq.respond(error(429, "blocked"));
        return;
    }
    // without sign-in nobody may tie up the workers: one pairing request per address, two at a time overall
    let (Some(_one), Some(_all)) = (st.pairing_per_peer.try_enter(key), st.pairing.try_enter()) else {
        let _ = rq.respond(error(503, "busy").with_header(header("Retry-After", "1")));
        return;
    };
    let body = match read_body(&mut rq) {
        Ok(b) => b,
        Err(s) => {
            // a broken or oversized body counts as a failed attempt (else it could be repeated without limit)
            st.sign_in.fail(key);
            st.log.write(peer, None, "POST", url, s, "pairing failed");
            let _ = rq.respond(error(s, "body"));
            return;
        }
    };
    let code = body.get("code").and_then(Value::as_str).unwrap_or("");
    let name = body.get("name").and_then(Value::as_str).unwrap_or("Browser");
    match st.devices.pair(code, name, st.device_days, key) {
        Ok((dev, token)) => {
            st.sign_in.success(key);
            st.log.write(peer, Some(&dev.id), "POST", url, 200, "paired");
            st.hooks.changed(Change::Devices);
            let max_age = if st.device_days > 0 { st.device_days * 86_400 } else { 10 * 365 * 86_400 };
            let r = json_response(200, &json!({ "device": dev.id, "role": dev.role, "csrf": dev.csrf }))
                .with_header(header("Set-Cookie", &format!("{COOKIE}={token}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Strict")));
            let _ = rq.respond(r);
        }
        Err(e) => {
            st.sign_in.fail(key);
            st.log.write(peer, None, "POST", url, 403, "pairing failed");
            let code = match e {
                PairError::NoCode => "no_code",
                PairError::Wrong => "wrong_code",
                PairError::TooManyDevices => "too_many_devices",
            };
            let _ = rq.respond(error(403, code));
        }
    }
}

fn jpeg(body: Arc<Vec<u8>>, cache: bool) -> Response<std::io::Cursor<Vec<u8>>> {
    secure(Response::from_data(body.as_ref().clone()).with_header(header("Content-Type", "image/jpeg")), cache)
}

fn respond(rq: Request, r: Response<std::io::Cursor<Vec<u8>>>) -> u16 {
    let s = r.status_code().0;
    let _ = rq.respond(r);
    s
}

fn route(st: &State, mut rq: Request, peer: IpAddr, dev: &Device, method: &Method, path: &str, url: &str) -> u16 {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let write_role = dev.role == Role::Write;
    match (method, parts.as_slice()) {
        (Method::Get, ["api", "session"]) => respond(rq, json_response(200, &json!({ "device": { "id": dev.id, "name": dev.name, "role": dev.role }, "csrf": dev.csrf, "expires": dev.expires }))),
        (Method::Post, ["api", "logout"]) => {
            st.devices.revoke(&dev.id);
            st.hooks.changed(Change::Devices);
            // the previews in the browser cache go too (shared tablet)
            respond(
                rq,
                json_response(200, &json!({ "ok": true }))
                    .with_header(header("Set-Cookie", &format!("{COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict")))
                    .with_header(header("Clear-Site-Data", "\"cache\", \"storage\"")),
            )
        }
        (Method::Get, ["api", "sources"]) => match catalog::sources() {
            Ok(v) => respond(rq, json_response(200, &v)),
            Err(e) => respond(rq, error(500, &short(&e))),
        },
        (Method::Get, ["api", "images"]) => {
            let mut q = catalog::Query { limit: 200, ..Default::default() };
            for (k, v) in query_params(url) {
                match k.as_str() {
                    "src" => q.source = v,
                    "q" => q.text = v.chars().take(200).collect(),
                    "min" => q.min_stars = v.parse().unwrap_or(0).min(5),
                    "flag" => q.flag = v,
                    "label" => q.label = v.parse().unwrap_or(0).min(5),
                    "sort" => q.sort = v,
                    "offset" => q.offset = v.parse().unwrap_or(0),
                    "limit" => q.limit = v.parse().unwrap_or(200),
                    _ => {}
                }
            }
            match catalog::images(&q) {
                Ok(v) => respond(rq, json_response(200, &v)),
                Err(e) if e == "bad source" || e == "unknown collection" => respond(rq, error(400, "source")),
                Err(e) => respond(rq, error(500, &short(&e))),
            }
        }
        (Method::Get, ["api", "image", id]) => {
            let Some(id) = parse_id(id) else { return respond(rq, error(404, "not_found")) };
            match catalog::details(id) {
                Ok(Some(mut v)) => {
                    v["editable"] = json!(write_role);
                    respond(rq, json_response(200, &v))
                }
                Ok(None) => respond(rq, error(404, "not_found")),
                Err(e) => respond(rq, error(500, &short(&e))),
            }
        }
        (Method::Get, ["api", "image", id, size @ ("thumb" | "large")]) => {
            let Some(id) = parse_id(id) else { return respond(rq, error(404, "not_found")) };
            let size = if *size == "thumb" { Size::Thumb } else { Size::Large };
            match catalog::preview_jpeg(id, size, &st.previews) {
                Ok(b) => respond(rq, jpeg(b, true)),
                Err(PreviewError::NotFound) => respond(rq, error(404, "not_found")),
                Err(PreviewError::Busy) => respond(rq, error(503, "busy").with_header(header("Retry-After", "1"))),
                Err(PreviewError::Failed(e)) => respond(rq, error(500, &short(&e))),
            }
        }
        (Method::Post, ["api", "image", id, "rating"]) => {
            if !write_role {
                return respond(rq, error(403, "read_only"));
            }
            if !st.writes.take(net::limit_key(peer), 1.0) {
                return respond(rq, error(429, "rate_limited"));
            }
            let Some(id) = parse_id(id) else { return respond(rq, error(404, "not_found")) };
            let body = match read_body(&mut rq) {
                Ok(b) => b,
                Err(s) => return respond(rq, error(s, "body")),
            };
            match rate(st, id, &body) {
                Ok(v) => respond(rq, json_response(200, &v)),
                Err((s, c)) => respond(rq, error(s, &c)),
            }
        }
        (Method::Get, ["api", "image", id, "develop"]) => {
            if !write_role {
                return respond(rq, error(403, "read_only"));
            }
            let Some(id) = parse_id(id) else { return respond(rq, error(404, "not_found")) };
            match develop_get(st, id) {
                Ok(v) => respond(rq, json_response(200, &v)),
                Err((s, c)) => respond(rq, error(s, &c)),
            }
        }
        (Method::Post, ["api", "image", id, action @ ("render" | "develop")]) => {
            if !write_role {
                return respond(rq, error(403, "read_only"));
            }
            let Some(id) = parse_id(id) else { return respond(rq, error(404, "not_found")) };
            if *action == "develop" && !st.writes.take(net::limit_key(peer), 1.0) {
                return respond(rq, error(429, "rate_limited"));
            }
            let body = match read_body(&mut rq) {
                Ok(b) => b,
                Err(s) => return respond(rq, error(s, "body")),
            };
            let result = if *action == "render" { develop_render(st, id, &body).map(|b| jpeg(Arc::new(b), false)) } else { develop_save(st, id, &body).map(|v| json_response(200, &v)) };
            match result {
                Ok(r) => respond(rq, r),
                Err((s, c)) => {
                    let mut r = error(s, &c);
                    if s == 503 {
                        r = r.with_header(header("Retry-After", "1"));
                    }
                    respond(rq, r)
                }
            }
        }
        _ => respond(rq, error(404, "not_found")),
    }
}

/// Error text for the browser: short, without paths.
fn short(e: &str) -> String {
    let first: String = e.lines().next().unwrap_or("").chars().take(120).collect();
    if first.contains('/') || first.contains('\\') {
        "failed".into()
    } else {
        first
    }
}

type Fail = (u16, String);

fn rate(st: &State, image: i64, body: &Value) -> Result<Value, Fail> {
    // one rating change at a time: read, change one field, write (two quick taps, e.g. stars then label, would
    // otherwise both start from the old values and the second would undo the first)
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let snap = catalog::snapshot().map_err(|e| (500, short(&e)))?;
    let z = snap.row(image).ok_or((404, "not_found".into()))?;
    let (mut s, mut f, mut l) = library::store::meta(z.file, z.copy.as_deref()).unwrap_or_else(|| snap.rating(z));
    let obj = body.as_object().ok_or((400, "body".into()))?;
    for k in obj.keys() {
        if !matches!(k.as_str(), "stars" | "flag" | "label") {
            return Err((400, "unknown_field".into()));
        }
    }
    if let Some(v) = obj.get("stars") {
        s = v.as_u64().filter(|x| *x <= 5).ok_or((400, "stars".into()))? as u8;
    }
    if let Some(v) = obj.get("flag") {
        f = v.as_i64().filter(|x| (-1..=1).contains(x)).ok_or((400, "flag".into()))? as i8;
    }
    if let Some(v) = obj.get("label") {
        l = v.as_u64().filter(|x| *x <= 5).ok_or((400, "label".into()))? as u8;
    }
    // the library's own write path (overlay, one writer thread, sidecar only if that option is on); the camera
    // JPEG of a RAW+JPEG pair gets the same rating, as in the stacked grid of the app
    let mut targets = vec![(image, z.file, z.copy.clone())];
    if let Some(j) = z.sidecar_jpeg.and_then(|j| snap.row(j)) {
        targets.push((j.image, j.file, j.copy.clone()));
    }
    library::store::batch(|| {
        for (_, file, copy) in &targets {
            library::store::set_meta(*file, copy.as_deref(), (s, f, l));
        }
    });
    for (img, _, _) in &targets {
        catalog::patch_rating(*img, (s, f, l));
        st.hooks.changed(Change::Rating { image: *img, stars: s, flag: f, label: l });
    }
    Ok(json!({ "stars": s, "flag": f, "label": l }))
}

fn kind_of(format: &str) -> Option<Kind> {
    match format {
        "X3F" => Some(Kind::Sigma),
        "VIDEO" => None,
        "JPG" | "JPEG" | "HEIC" | "HEIF" | "TIF" | "TIFF" | "PNG" => Some(Kind::Finished),
        _ => Some(Kind::Raw),
    }
}

struct Target {
    path: PathBuf,
    file: i64,
    copy: Option<String>,
    kind: Kind,
}

fn target(image: i64) -> Result<Target, Fail> {
    let snap = catalog::snapshot().map_err(|e| (500, short(&e)))?;
    let z = snap.row(image).ok_or((404, "not_found".into()))?;
    let kind = kind_of(&z.format).ok_or((409, "not_developable".into()))?;
    let path = snap.path_of(z);
    if !path.is_file() {
        return Err((409, "original_unreachable".into()));
    }
    Ok(Target { path, file: z.file, copy: z.copy.clone(), kind })
}

/// Version mark of a settings state (the browser sends it back when saving).
fn version_of(s: &develop::Settings) -> String {
    use sha2::Digest;
    let j = serde_json::to_string(s).unwrap_or_default();
    sha2::Sha256::digest(j.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn develop_get(st: &State, image: i64) -> Result<Value, Fail> {
    let t = target(image)?;
    let s = st.hooks.load_settings(&t.path, t.copy.as_deref()).map_err(|e| (500, short(&e)))?;
    let wb = if t.kind == Kind::Sigma { None } else { st.hooks.white_balance(&t.path, &s) };
    let sliders: Vec<Value> = develop_edit::sliders(t.kind).iter().map(|s| json!({ "key": s.key, "min": s.min, "max": s.max, "step": s.step })).collect();
    Ok(json!({
        "kind": t.kind.name(),
        "values": develop_edit::values(t.kind, &s, wb),
        "sliders": sliders,
        "wb_modes": if t.kind == Kind::Raw { json!(develop_edit::RAW_WB_MODES) } else { json!([]) },
        "version": version_of(&s),
        "open_in_app": st.hooks.is_open(t.file, t.copy.as_deref()),
    }))
}

fn edited(st: &State, t: &Target, body: &Value) -> Result<(develop::Settings, develop::Settings), Fail> {
    let before = st.hooks.load_settings(&t.path, t.copy.as_deref()).map_err(|e| (500, short(&e)))?;
    let values = body.get("values").ok_or((400, "values".into()))?;
    let wb = if t.kind == Kind::Sigma { None } else { st.hooks.white_balance(&t.path, &before) };
    let mut after = before.clone();
    develop_edit::apply(t.kind, &mut after, values, wb).map_err(|e| (400, e))?;
    Ok((before, after))
}

fn develop_render(st: &State, image: i64, body: &Value) -> Result<Vec<u8>, Fail> {
    // the slot first: reading the settings can open the whole RAW file
    let Some(_slot) = st.renders.try_enter() else { return Err((503, "busy".into())) };
    let t = target(image)?;
    let (_, after) = edited(st, &t, body)?;
    let img = st.hooks.render(&t.path, &after, RENDER_LEVEL).map_err(|e| (500, short(&e)))?;
    catalog::encode_jpeg(&img, 85).map_err(|e| (500, e))
}

fn develop_save(st: &State, image: i64, body: &Value) -> Result<Value, Fail> {
    // version check and saving in one step (two devices cannot both pass the check)
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let t = target(image)?;
    if st.hooks.is_open(t.file, t.copy.as_deref()) {
        return Err((409, "open_in_app".into()));
    }
    let (before, after) = edited(st, &t, body)?;
    // someone changed the photo since the browser loaded it (graineo on the Mac, another device)
    let expected = body.get("version").and_then(Value::as_str).unwrap_or("");
    if expected != version_of(&before) {
        return Err((409, "changed_elsewhere".into()));
    }
    if before == after {
        return Ok(json!({ "saved": false, "version": version_of(&before) }));
    }
    st.hooks.save_settings(&t.path, t.file, t.copy.as_deref(), &before, &after).map_err(|e| (500, short(&e)))?;
    catalog::forget(image);
    // the new development for the grid of the app (and the next thumbnail here), made in the background
    after_save(st, AfterSave { image, file: t.file, copy: t.copy.clone(), path: t.path.clone(), settings: after.clone() });
    Ok(json!({ "saved": true, "version": version_of(&after) }))
}

struct AfterSave {
    image: i64,
    file: i64,
    copy: Option<String>,
    path: PathBuf,
    settings: develop::Settings,
}

/// Queues the development after a save for one background thread. When the queue is full (a client saving very
/// fast), the app is told without a preview and develops the photo itself when it shows it.
fn after_save(st: &State, mut job: AfterSave) {
    const QUEUE: usize = 4;
    let mut g = st.after_save.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<AfterSave>(QUEUE);
        let hooks = st.hooks.clone();
        let spawned = std::thread::Builder::new().name("remote-after-save".into()).spawn(move || {
            library::platform::background_priority();
            while let Ok(j) = rx.recv() {
                let preview = hooks.render(&j.path, &j.settings, 1280).ok().map(Arc::new);
                hooks.changed(Change::Develop { image: j.image, file: j.file, copy: j.copy, preview });
            }
        });
        if spawned.is_ok() {
            *g = Some(tx);
        }
    }
    if let Some(tx) = g.as_ref() {
        match tx.try_send(job) {
            Ok(()) => return,
            Err(std::sync::mpsc::TrySendError::Full(j) | std::sync::mpsc::TrySendError::Disconnected(j)) => job = j,
        }
    }
    st.hooks.changed(Change::Develop { image: job.image, file: job.file, copy: job.copy, preview: None });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_digits_only() {
        assert_eq!(parse_id("42"), Some(42));
        for bad in ["", "..", "%2e%2e", "1/2", "-1", "1e3", "00000000000000000000001", "1;rm"] {
            assert_eq!(parse_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c").as_deref(), Some("a b c"));
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%2"), None);
        assert_eq!(percent_decode("%C3%BC").as_deref(), Some("ü"));
    }
}
