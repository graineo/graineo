//! TLS front: accepts HTTPS connections in the home network, terminates TLS with rustls and
//! passes the plain HTTP stream to the internal `tiny_http` server on the loopback interface.
//!
//! The internal server only answers connections that this front opened itself: the front
//! records "local port of the loopback connection → address of the real peer" before it
//! forwards a single byte, and the request handler rejects every connection that is not in
//! that table (another program on this computer cannot bypass TLS or the address checks).

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::net;

/// Most connections at the same time (all peers) and per peer address.
const MAX_CONNECTIONS: usize = 64;
const MAX_PER_PEER: usize = 16;
/// A connection without any traffic for this long is closed.
const IDLE: Duration = Duration::from_secs(60);
/// … and only this long while more than half of [`MAX_CONNECTIONS`] are open (idle keep-alive connections, e.g. of
/// a client that holds many of them open on purpose, make room for new ones).
const IDLE_CROWDED: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Peers {
    map: Mutex<HashMap<u16, IpAddr>>,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
}

impl Peers {
    /// Real address of the peer behind an internal loopback connection.
    pub fn lookup(&self, internal: SocketAddr) -> Option<IpAddr> {
        // the front connects from 127.0.0.1 exactly (another loopback address with the same port is not it)
        if internal.ip() != IpAddr::from([127, 0, 0, 1]) {
            return None;
        }
        self.map.lock().unwrap().get(&internal.port()).copied()
    }
}

pub struct Front {
    pub peers: Arc<Peers>,
    pub stop: Arc<AtomicBool>,
    pub addr: SocketAddr,
    /// disconnects when the accept loop has ended and closed the listening socket
    accept_done: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

/// Starts the accept loop on `listener`. `allow_outside` decides at connect time whether peers
/// outside the home network are accepted.
pub fn start(
    listener: TcpListener,
    internal: SocketAddr,
    tls: Arc<rustls::ServerConfig>,
    allow_outside: Arc<AtomicBool>,
) -> std::io::Result<Front> {
    let addr = listener.local_addr()?;
    let peers = Arc::new(Peers::default());
    let stop = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicUsize::new(0));
    let (p, s) = (peers.clone(), stop.clone());
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new().name("remote-accept".into()).spawn(move || {
        // (locals drop in reverse order: the listener closes before `_done` reports the end)
        let _done = done_tx;
        let listener = listener;
        for conn in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            let Ok(tcp) = conn else { continue };
            let Ok(peer) = tcp.peer_addr() else { continue };
            let ip = canonical(peer.ip());
            if !net::is_local_network(ip) && !allow_outside.load(Ordering::SeqCst) {
                let _ = tcp.shutdown(Shutdown::Both);
                continue;
            }
            if count.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                let _ = tcp.shutdown(Shutdown::Both);
                continue;
            }
            {
                let mut per = p.per_ip.lock().unwrap();
                let n = per.entry(net::limit_key(ip)).or_insert(0);
                if *n >= MAX_PER_PEER {
                    let _ = tcp.shutdown(Shutdown::Both);
                    continue;
                }
                *n += 1;
            }
            count.fetch_add(1, Ordering::SeqCst);
            let (p2, c2, tls2, s2) = (p.clone(), count.clone(), tls.clone(), s.clone());
            let spawned = std::thread::Builder::new().name("remote-conn".into()).spawn(move || {
                let _ = serve(tcp, ip, internal, tls2, &p2, &s2, &c2);
                let mut per = p2.per_ip.lock().unwrap();
                let key = net::limit_key(ip);
                if let Some(n) = per.get_mut(&key) {
                    *n -= 1;
                    if *n == 0 {
                        per.remove(&key);
                    }
                }
                c2.fetch_sub(1, Ordering::SeqCst);
            });
            if spawned.is_err() {
                count.fetch_sub(1, Ordering::SeqCst);
            }
        }
    })?;
    Ok(Front { peers, stop, addr, accept_done: Mutex::new(Some(done_rx)) })
}

impl Front {
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the blocking accept
        let wake: SocketAddr = match self.addr.ip() {
            ip if ip.is_unspecified() => SocketAddr::new(IpAddr::from([127, 0, 0, 1]), self.addr.port()),
            _ => self.addr,
        };
        let _ = TcpStream::connect_timeout(&wake, Duration::from_millis(300));
        // the port is free again once the accept loop has ended (a restart binds it at once); short wait only
        if let Some(rx) = self.accept_done.lock().unwrap().take() {
            let _ = rx.recv_timeout(Duration::from_secs(1));
        }
    }
}

/// IPv4-mapped IPv6 addresses as plain IPv4.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

fn serve(
    tcp: TcpStream,
    ip: IpAddr,
    internal: SocketAddr,
    tls: Arc<rustls::ServerConfig>,
    peers: &Peers,
    stop: &AtomicBool,
    open: &AtomicUsize,
) -> std::io::Result<()> {
    tcp.set_read_timeout(Some(IDLE))?;
    tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
    tcp.set_nodelay(true)?;
    let conn = rustls::ServerConnection::new(tls).map_err(std::io::Error::other)?;
    let backend = TcpStream::connect_timeout(&internal, Duration::from_secs(2))?;
    let local_port = backend.local_addr()?.port();
    peers.map.lock().unwrap().insert(local_port, ip);
    let result = pump(tcp, backend, conn, stop, open);
    peers.map.lock().unwrap().remove(&local_port);
    result
}

/// Largest request head and body the front passes on; a request must arrive completely (within
/// [`REQUEST_TIME`]) before tiny_http sees a byte of it, so a slow client never ties up a worker.
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;
const REQUEST_TIME: Duration = Duration::from_secs(20);
const HANDSHAKE_TIME: Duration = Duration::from_secs(10);
const LIFETIME: Duration = Duration::from_secs(600);
/// Answer chunks (up to 64 KB each, so at most 2 MB) buffered per connection while the client reads slowly (the
/// worker is free meanwhile).
const OUT_CHUNKS: usize = 32;
/// Most requests of one connection passed on to the internal server and not answered completely yet. tiny_http hands
/// pipelined requests of one connection to several workers at once, and each answer waits for the one before it: a
/// client that sends many requests and reads nothing would otherwise tie up every worker.
const MAX_IN_FLIGHT: usize = 2;
/// The next request is only passed on while less than this many answer bytes wait for the client (with
/// [`MAX_IN_FLIGHT`] answers of the static files the buffer never fills, so a worker never waits for this client).
const OUT_LIMIT: usize = 256 * 1024;
/// The buffer stays full this long (the client takes nothing): both sides of the connection are closed, which frees a
/// worker that is writing a large answer.
const STALL: Duration = Duration::from_secs(5);

/// Most header lines of one request.
const MAX_HEADERS: usize = 64;

#[derive(Debug, PartialEq)]
enum Framing {
    /// a complete request: `consumed` bytes at the start of the buffer, passed on as `forward` (normalized head + body)
    Ready { consumed: usize, forward: Vec<u8> },
    Incomplete,
    /// answer this status and close
    Refuse(u16, &'static str),
}

const BAD: Framing = Framing::Refuse(400, "Bad Request");

/// RFC 9110 token characters (header names).
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Headers that must come at most once: tiny_http and the handler use the first, other readers the last, and they
/// steer the framing or the checks of the handler (host, origin, sign-in, CSRF).
const SINGLE: [&str; 7] = ["host", "content-length", "connection", "origin", "content-type", "cookie", "x-graineo-csrf"];

/// Checks one request head strictly and decides its length itself. The internal server only ever sees a head this
/// function wrote (one `Content-Length`, no `Transfer-Encoding`, no `Expect`, no `Upgrade`, clean CRLF lines), so it
/// cannot see a different request boundary than the front (request smuggling) or wait for bytes that never come.
fn frame(buf: &[u8]) -> Framing {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() > MAX_HEAD { Framing::Refuse(431, "Request Header Fields Too Large") } else { Framing::Incomplete };
    };
    if end > MAX_HEAD {
        return Framing::Refuse(431, "Request Header Fields Too Large");
    }
    let head = &buf[..end];
    // printable ASCII and tabs only; CR only before LF and LF only after CR (no bare line ends that two parsers
    // would split differently)
    for (i, &b) in head.iter().enumerate() {
        let ok = match b {
            b'\r' => head.get(i + 1) == Some(&b'\n'),
            b'\n' => i > 0 && head[i - 1] == b'\r',
            b'\t' | 0x20..=0x7e => true,
            _ => false,
        };
        if !ok {
            return BAD;
        }
    }
    let Ok(head) = std::str::from_utf8(head) else { return BAD };
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = request_line.split(' ').collect();
    let [method, target, version] = parts.as_slice() else { return BAD };
    if method.is_empty() || method.len() > 16 || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return BAD;
    }
    if !target.starts_with('/') || target.contains('\t') {
        return BAD;
    }
    if *version != "HTTP/1.1" && *version != "HTTP/1.0" {
        return Framing::Refuse(505, "HTTP Version Not Supported");
    }
    let mut out = format!("{method} {target} {version}\r\n");
    let mut length = 0usize;
    let mut seen: Vec<&'static str> = Vec::new();
    for (count, line) in lines.enumerate() {
        if count >= MAX_HEADERS {
            return Framing::Refuse(431, "Request Header Fields Too Large");
        }
        // obsolete line folding
        if line.starts_with([' ', '\t']) {
            return BAD;
        }
        let Some((name, value)) = line.split_once(':') else { return BAD };
        if name.is_empty() || !name.bytes().all(is_tchar) {
            return BAD;
        }
        let value = value.trim_matches([' ', '\t']);
        let lower = name.to_ascii_lowercase();
        if let Some(s) = SINGLE.iter().find(|s| **s == lower) {
            if seen.contains(s) {
                return BAD;
            }
            seen.push(s);
        }
        match lower.as_str() {
            "transfer-encoding" | "upgrade" => return BAD,
            "expect" => return Framing::Refuse(417, "Expectation Failed"),
            // tiny_http hands the whole rest of the stream to the handler for anything containing "upgrade"
            "connection" if value.to_ascii_lowercase().contains("upgrade") => return BAD,
            "content-length" => {
                if value.is_empty() || value.len() > 10 || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return BAD;
                }
                let n: usize = value.parse().unwrap_or(usize::MAX);
                if n > MAX_BODY {
                    return Framing::Refuse(413, "Payload Too Large");
                }
                length = n;
                out.push_str(&format!("Content-Length: {n}\r\n"));
                continue;
            }
            _ => {}
        }
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    let total = end + 4 + length;
    if buf.len() < total {
        return Framing::Incomplete;
    }
    let mut forward = out.into_bytes();
    forward.extend_from_slice(&buf[end + 4..total]);
    Framing::Ready { consumed: total, forward }
}

/// End of each answer in the stream of the internal server (HTTP/1.1 framing: `Content-Length`, chunked, no body
/// for HEAD, 1xx, 204 and 304), so that the front knows how many requests are still unanswered. Anything it does not
/// understand switches the count off ([`Answers::lost`]); the front then passes requests on as before.
#[derive(Default)]
struct Answers {
    /// one entry per request passed on and not answered completely: `true` for HEAD
    waiting: std::collections::VecDeque<bool>,
    state: AnswerState,
    /// bytes of the current head or chunk-size/trailer line
    line: Vec<u8>,
    lost: bool,
}

#[derive(Default, Debug, PartialEq)]
enum AnswerState {
    #[default]
    Head,
    Body(u64),
    ChunkSize,
    ChunkData(u64),
    /// CRLF after chunk data: bytes still expected
    ChunkEnd(u8),
    Trailer,
}

impl Answers {
    fn lose(&mut self) {
        self.lost = true;
        self.waiting.clear();
        self.line.clear();
    }

    fn done(&mut self) {
        self.waiting.pop_front();
        self.state = AnswerState::Head;
    }

    /// Feeds bytes of the internal server's stream.
    fn feed(&mut self, mut data: &[u8]) {
        while !data.is_empty() && !self.lost {
            match self.state {
                AnswerState::Head => {
                    let take = match data.iter().position(|&b| b == b'\n') {
                        Some(k) => k + 1,
                        None => data.len(),
                    };
                    self.line.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if self.line.len() > 64 * 1024 {
                        self.lose();
                    } else if self.line.ends_with(b"\r\n\r\n") {
                        let head = std::mem::take(&mut self.line);
                        self.head(&head);
                    }
                }
                AnswerState::Body(n) => {
                    let k = (data.len() as u64).min(n);
                    data = &data[k as usize..];
                    if n == k {
                        self.done();
                    } else {
                        self.state = AnswerState::Body(n - k);
                    }
                }
                AnswerState::ChunkSize | AnswerState::Trailer => {
                    let take = match data.iter().position(|&b| b == b'\n') {
                        Some(k) => k + 1,
                        None => data.len(),
                    };
                    self.line.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if self.line.len() > 4096 {
                        self.lose();
                    } else if self.line.ends_with(b"\r\n") {
                        let line = std::mem::take(&mut self.line);
                        let text = &line[..line.len() - 2];
                        if self.state == AnswerState::Trailer {
                            if text.is_empty() {
                                self.done();
                            }
                            continue;
                        }
                        let hex = std::str::from_utf8(text).ok().map(|t| t.split(';').next().unwrap_or("").trim());
                        match hex.and_then(|h| u64::from_str_radix(h, 16).ok()) {
                            Some(0) => self.state = AnswerState::Trailer,
                            Some(n) => self.state = AnswerState::ChunkData(n),
                            None => self.lose(),
                        }
                    }
                }
                AnswerState::ChunkData(n) => {
                    let k = (data.len() as u64).min(n);
                    data = &data[k as usize..];
                    self.state = if n == k { AnswerState::ChunkEnd(2) } else { AnswerState::ChunkData(n - k) };
                }
                AnswerState::ChunkEnd(left) => {
                    let want: &[u8] = if left == 2 { b"\r" } else { b"\n" };
                    if data[0] != want[0] {
                        self.lose();
                        break;
                    }
                    data = &data[1..];
                    self.state = if left == 2 { AnswerState::ChunkEnd(1) } else { AnswerState::ChunkSize };
                }
            }
        }
    }

    /// A complete answer head: decides how its body ends.
    fn head(&mut self, head: &[u8]) {
        let Ok(text) = std::str::from_utf8(head) else { return self.lose() };
        let mut lines = text.split("\r\n");
        let status: Option<u16> = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|c| c.parse().ok());
        let Some(status) = status else { return self.lose() };
        if (100..200).contains(&status) {
            // an interim answer: the final one follows
            return;
        }
        let is_head = self.waiting.front().copied().unwrap_or(false);
        let (mut length, mut chunked) = (None, false);
        for line in lines {
            let Some((name, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                match value.parse::<u64>() {
                    Ok(n) => length = Some(n),
                    Err(_) => return self.lose(),
                }
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                if !value.eq_ignore_ascii_case("chunked") {
                    return self.lose();
                }
                chunked = true;
            }
        }
        if is_head || status == 204 || status == 304 {
            self.done();
        } else if chunked {
            self.state = AnswerState::ChunkSize;
        } else if let Some(n) = length {
            if n == 0 {
                self.done();
            } else {
                self.state = AnswerState::Body(n);
            }
        } else {
            // body until the connection closes: nothing more to count
            self.lose();
        }
    }
}

/// What the request side of a connection needs to know about its answer side.
#[derive(Default)]
struct Gate {
    answers: Mutex<Answers>,
    changed: std::sync::Condvar,
    /// answer bytes read from the internal server and not yet written to the client
    queued: AtomicUsize,
    /// the answer side has ended (client gone, internal server closed, or the client stalled)
    ended: AtomicBool,
}

impl Gate {
    fn full(&self, a: &Answers) -> bool {
        self.queued.load(Ordering::SeqCst) >= OUT_LIMIT || (!a.lost && a.waiting.len() >= MAX_IN_FLIGHT)
    }

    /// Registers a request about to be passed on; `false`: not now (too many unanswered or too much unread).
    fn admit(&self, is_head: bool) -> bool {
        let mut a = self.answers.lock().unwrap();
        if self.full(&a) {
            return false;
        }
        if !a.lost {
            a.waiting.push_back(is_head);
        }
        true
    }

    /// Waits (at most `d`) until another request may be passed on or the answer side ended.
    fn wait(&self, d: Duration) {
        let a = self.answers.lock().unwrap();
        if self.full(&a) && !self.ended.load(Ordering::SeqCst) {
            let _ = self.changed.wait_timeout(a, d);
        }
    }

    fn notify(&self) {
        drop(self.answers.lock().unwrap());
        self.changed.notify_all();
    }

    fn end(&self) {
        self.ended.store(true, Ordering::SeqCst);
        self.notify();
    }
}

fn pump(tcp: TcpStream, backend: TcpStream, conn: rustls::ServerConnection, stop: &AtomicBool, open: &AtomicUsize) -> std::io::Result<()> {
    let mut tcp_read = tcp.try_clone()?;
    let tcp_close = tcp.try_clone()?;
    let mut back_write = backend.try_clone()?;
    let mut back_read = backend;
    back_read.set_read_timeout(Some(Duration::from_secs(300)))?;
    // short read timeout: the loop checks its deadlines regularly
    tcp_read.set_read_timeout(Some(Duration::from_secs(2)))?;
    let shared = Arc::new(Mutex::new((conn, tcp)));
    let gate = Arc::new(Gate::default());

    // backend → buffer → client: the backend side is read at once (tiny_http's worker is free again), the client
    // gets the bytes as fast as it reads them
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(OUT_CHUNKS);
    let g_back = gate.clone();
    let backend_reader = std::thread::Builder::new().name("remote-conn-back".into()).spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        'read: loop {
            let n = match back_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            g_back.answers.lock().unwrap().feed(&buf[..n]);
            g_back.queued.fetch_add(n, Ordering::SeqCst);
            g_back.notify();
            let mut chunk = buf[..n].to_vec();
            let since = Instant::now();
            loop {
                match tx.try_send(chunk) {
                    Ok(()) => break,
                    Err(std::sync::mpsc::TrySendError::Full(c)) if since.elapsed() < STALL => {
                        chunk = c;
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {
                        // the client takes nothing: close both sides, so the worker writing to us is free at once
                        let _ = back_read.shutdown(Shutdown::Both);
                        let _ = tcp_close.shutdown(Shutdown::Both);
                        break 'read;
                    }
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break 'read,
                }
            }
        }
        g_back.end();
    })?;
    let out = shared.clone();
    let g_out = gate.clone();
    // a refusal of the front is written after the answers to the requests before it (pipelining keeps its order)
    let refusal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let last_words = refusal.clone();
    let writer = std::thread::Builder::new().name("remote-conn-out".into()).spawn(move || {
        let mut ok = true;
        while let Ok(chunk) = rx.recv() {
            let mut g = out.lock().unwrap();
            let (c, t) = &mut *g;
            if c.writer().write_all(&chunk).is_err() || flush_tls(c, t).is_err() {
                ok = false;
                break;
            }
            drop(g);
            g_out.queued.fetch_sub(chunk.len(), Ordering::SeqCst);
            g_out.notify();
        }
        g_out.end();
        let mut g = out.lock().unwrap();
        let (c, t) = &mut *g;
        if let Some(text) = last_words.lock().unwrap().take().filter(|_| ok) {
            let _ = c.writer().write_all(text.as_bytes());
        }
        c.send_close_notify();
        let _ = flush_tls(c, t);
        let _ = t.shutdown(Shutdown::Both);
    })?;

    // client → backend, whole requests only
    let started = Instant::now();
    let mut last = Instant::now();
    let mut request_since: Option<Instant> = None;
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 32 * 1024];
    let mut plain = vec![0u8; 32 * 1024];
    let mut closed = false;
    let refuse = |status: u16, text: &str| {
        *refusal.lock().unwrap() = Some(format!("HTTP/1.1 {status} {text}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"));
    };
    let result: std::io::Result<()> = (|| loop {
        if stop.load(Ordering::SeqCst) || started.elapsed() > LIFETIME {
            return Ok(());
        }
        let handshaking = shared.lock().unwrap().0.is_handshaking();
        if handshaking && started.elapsed() > HANDSHAKE_TIME {
            return Ok(());
        }
        if request_since.is_some_and(|t| t.elapsed() > REQUEST_TIME) {
            refuse(408, "Request Timeout");
            return Ok(());
        }
        if gate.ended.load(Ordering::SeqCst) {
            return Ok(());
        }
        // pass on the complete requests, as far as the answer side keeps up
        let mut blocked = false;
        loop {
            match frame(&pending) {
                Framing::Ready { consumed, forward } => {
                    if !gate.admit(forward.starts_with(b"HEAD ")) {
                        blocked = true;
                        break;
                    }
                    back_write.write_all(&forward)?;
                    pending.drain(..consumed);
                }
                Framing::Incomplete => break,
                Framing::Refuse(status, text) => {
                    refuse(status, text);
                    return Ok(());
                }
            }
        }
        // a request waiting for earlier answers is not a slow client
        request_since = if pending.is_empty() || blocked { None } else { request_since.or(Some(Instant::now())) };
        if closed {
            return Ok(());
        }
        if blocked {
            // read nothing more meanwhile (the buffer of requests stays bounded)
            gate.wait(Duration::from_secs(1));
            continue;
        }
        let n = match tcp_read.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                let idle = if open.load(Ordering::SeqCst) > MAX_CONNECTIONS / 2 { IDLE_CROWDED } else { IDLE };
                if last.elapsed() > idle && pending.is_empty() {
                    return Ok(());
                }
                continue;
            }
            Err(e) => return Err(e),
        };
        last = Instant::now();
        {
            let mut g = shared.lock().unwrap();
            let (c, t) = &mut *g;
            let mut slice = &buf[..n];
            while !slice.is_empty() {
                // 0: rustls takes nothing more (the client closed the TLS session)
                if c.read_tls(&mut slice)? == 0 {
                    closed = true;
                    break;
                }
                if let Err(e) = c.process_new_packets() {
                    let _ = flush_tls(c, t);
                    return Err(std::io::Error::other(e));
                }
                loop {
                    match c.reader().read(&mut plain) {
                        Ok(0) => {
                            closed = true;
                            break;
                        }
                        Ok(k) => pending.extend_from_slice(&plain[..k]),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e),
                    }
                }
                if closed || pending.len() > MAX_HEAD + MAX_BODY {
                    break;
                }
            }
            flush_tls(c, t)?;
        }
        request_since = if pending.is_empty() { None } else { request_since.or(Some(Instant::now())) };
    })();
    // after a refusal the internal server finishes the answers it is working on (it sees the end of the requests),
    // otherwise the connection ends at once
    let refused = refusal.lock().unwrap().is_some();
    let _ = back_write.shutdown(if refused { Shutdown::Write } else { Shutdown::Both });
    let _ = backend_reader.join();
    let _ = writer.join();
    result
}

fn flush_tls(c: &mut rustls::ServerConnection, t: &mut TcpStream) -> std::io::Result<()> {
    while c.wants_write() {
        c.write_tls(t)?;
    }
    t.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(f: Framing) -> (usize, String) {
        match f {
            Framing::Ready { consumed, forward } => (consumed, String::from_utf8(forward).unwrap()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn complete_requests_are_passed_on_normalized() {
        let raw = b"POST /api/pair HTTP/1.1\r\nHost:  10.0.0.2:47820 \r\nContent-Length: 007\r\n\r\n{\"a\":1}GET / HTTP/1.1\r\n";
        let (consumed, fwd) = ready(frame(raw));
        assert_eq!(consumed, raw.len() - "GET / HTTP/1.1\r\n".len());
        assert_eq!(fwd, "POST /api/pair HTTP/1.1\r\nHost: 10.0.0.2:47820\r\nContent-Length: 7\r\n\r\n{\"a\":1}");
        assert_eq!(frame(b"GET / HTTP/1.1\r\nHost: a\r\n"), Framing::Incomplete);
        assert_eq!(frame(b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab"), Framing::Incomplete);
    }

    #[test]
    fn ambiguous_framing_is_refused() {
        for (head, status) in [
            // two lengths: tiny_http takes the first, a naive reader the last
            ("POST /api/pair HTTP/1.1\r\nContent-Length: 2000\r\nContent-Length: 0\r\n\r\n", 400),
            ("POST /api/pair HTTP/1.1\r\nContent-Length: 0\r\ncontent-length: 0\r\n\r\n", 400),
            ("POST / HTTP/1.1\r\nContent-Length: +5\r\n\r\nhello", 400),
            ("POST / HTTP/1.1\r\nContent-Length: 5 5\r\n\r\nhello", 400),
            ("POST / HTTP/1.1\r\nContent-Length: 99999999999\r\n\r\n", 400),
            ("POST / HTTP/1.1\r\nContent-Length: 2000000\r\n\r\n", 413),
            ("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n", 400),
            ("POST / HTTP/1.1\r\nTransfer-Encoding : chunked\r\n\r\n", 400),
            // tiny_http hands the rest of the stream to the handler
            ("POST /api/pair HTTP/1.1\r\nConnection: Upgrade\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nConnection: keep-alive, x-upgrade\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nConnection: close\r\nConnection: keep-alive\r\n\r\n", 400),
            ("POST / HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 1\r\n\r\n", 417),
            // line ends and lines two parsers read differently
            ("GET / HTTP/1.1\r\nX-A: 1\nContent-Length: 5\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nX-A: 1\rContent-Length: 5\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nno colon here\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nX-A: 1\r\n folded: yes\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nX A: 1\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\n: empty\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nX-A: a\x00b\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nX-A: \u{e4}\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n", 400),
            ("GET / HTTP/1.1\r\nOrigin: https://a\r\nOrigin: https://b\r\n\r\n", 400),
            // request line
            ("GET  / HTTP/1.1\r\n\r\n", 400),
            ("GET / HTTP/1.1 x\r\n\r\n", 400),
            ("GET http://a/ HTTP/1.1\r\n\r\n", 400),
            ("get / HTTP/1.1\r\n\r\n", 400),
            ("GET / HTTP/2.0\r\n\r\n", 505),
        ] {
            match frame(head.as_bytes()) {
                Framing::Refuse(s, _) => assert_eq!(s, status, "{head:?}"),
                other => panic!("{head:?}: {other:?}"),
            }
        }
        let many: String = (0..MAX_HEADERS + 1).map(|i| format!("X-{i}: 1\r\n")).collect();
        assert_eq!(frame(format!("GET / HTTP/1.1\r\n{many}\r\n").as_bytes()), Framing::Refuse(431, "Request Header Fields Too Large"));
    }

    #[test]
    fn a_get_without_length_has_no_body() {
        let (consumed, fwd) = ready(frame(b"GET /app.css HTTP/1.1\r\nHost: a\r\nConnection: keep-alive\r\n\r\nGET"));
        assert_eq!(consumed, "GET /app.css HTTP/1.1\r\nHost: a\r\nConnection: keep-alive\r\n\r\n".len());
        assert!(fwd.ends_with("\r\n\r\n"));
    }

    /// The end of every answer is found however the stream is cut: length, chunked (with extension and trailer),
    /// HEAD, 204/304 and an interim 100.
    #[test]
    fn answers_are_counted_in_any_cut() {
        let stream: Vec<u8> = [
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
            "HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n",
            "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;x=1\r\nabc\r\n10\r\n0123456789abcdef\r\n0\r\nX-T: 1\r\n\r\n",
            "HTTP/1.1 304 Not Modified\r\nContent-Length: 7\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 404 Not Found\r\ncontent-length: 2\r\n\r\nno",
        ]
        .concat()
        .into_bytes();
        // HEAD for the second request: its length names a body that is not sent
        let methods = [false, true, false, false, false, false];
        for cut in [1usize, 2, 3, 7, 64, stream.len()] {
            let mut a = Answers::default();
            a.waiting.extend(methods);
            let mut answered = vec![];
            for piece in stream.chunks(cut) {
                a.feed(piece);
                answered.push(methods.len() - a.waiting.len());
            }
            assert!(!a.lost, "cut {cut}");
            assert!(a.waiting.is_empty(), "cut {cut}: {} left", a.waiting.len());
            assert_eq!(a.state, AnswerState::Head);
            // never counted ahead of the bytes
            if cut == 1 {
                let first = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".len();
                assert_eq!(answered[first - 2], 0);
                assert_eq!(answered[first - 1], 1);
            }
        }
        // a body until the connection ends, or something unknown: counting stops (the front then passes on as before)
        for odd in ["HTTP/1.0 200 OK\r\n\r\nbody", "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n", "garbage\r\n\r\n"] {
            let mut a = Answers::default();
            a.waiting.push_back(false);
            a.feed(odd.as_bytes());
            assert!(a.lost, "{odd:?}");
        }
    }

    fn test_tls() -> (Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>) {
        let dir = std::env::temp_dir().join(format!("graineo-proxy-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let certs = crate::tls::load_or_create(&dir, &["127.0.0.1".to_string()]).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls::pki_types::CertificateDer::from(certs.ca_der.clone())).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let client = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
        let _ = std::fs::remove_dir_all(&dir);
        (certs.server_config, Arc::new(client))
    }

    /// tiny_http with 4 workers behind the front, answering `size` bytes (chunked above 32 KB, like the static files
    /// and previews).
    fn test_server(size: usize) -> (Front, Arc<tiny_http::Server>, Arc<AtomicBool>, Arc<rustls::ClientConfig>) {
        let (server_tls, client_tls) = test_tls();
        let http = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let internal = http.server_addr().to_ip().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let front = start(listener, internal, server_tls, Arc::new(AtomicBool::new(true))).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        for _ in 0..4 {
            let (srv, s) = (http.clone(), stop.clone());
            std::thread::spawn(move || {
                while !s.load(Ordering::SeqCst) {
                    if let Ok(Some(rq)) = srv.recv_timeout(Duration::from_millis(100)) {
                        let _ = rq.respond(tiny_http::Response::from_data(vec![b'x'; size]));
                    }
                }
            });
        }
        (front, http, stop, client_tls)
    }

    fn connect(front: &Front, client: &Arc<rustls::ClientConfig>) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
        let tcp = TcpStream::connect(front.addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let name = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();
        rustls::StreamOwned::new(rustls::ClientConnection::new(client.clone(), name).unwrap(), tcp)
    }

    const GET: &str = "GET /app.js HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";

    /// A client that pipelines many requests and never reads ties up no worker: another client is answered at once.
    #[test]
    fn a_client_that_does_not_read_blocks_nobody_else() {
        let (front, _http, stop, client) = test_server(31 * 1024);
        let mut greedy = connect(&front, &client);
        greedy.write_all(GET.repeat(300).as_bytes()).unwrap();
        greedy.flush().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let t = Instant::now();
        let mut other = connect(&front, &client);
        other.write_all(b"GET /app.js HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").unwrap();
        let mut answer = Vec::new();
        let _ = other.read_to_end(&mut answer);
        assert!(answer.starts_with(b"HTTP/1.1 200"), "{:?}", String::from_utf8_lossy(&answer[..answer.len().min(80)]));
        assert!(t.elapsed() < Duration::from_secs(3), "answered after {:?}", t.elapsed());
        drop(greedy);
        stop.store(true, Ordering::SeqCst);
        front.shutdown();
    }

    /// Large answers that nobody reads (one per worker) do not hold the workers longer than [`STALL`].
    #[test]
    fn stalled_large_answers_free_the_workers() {
        let (front, _http, stop, client) = test_server(48 * 1024 * 1024);
        let greedy: Vec<_> = (0..4)
            .map(|_| {
                let mut c = connect(&front, &client);
                c.write_all(GET.as_bytes()).unwrap();
                c.flush().unwrap();
                c
            })
            .collect();
        std::thread::sleep(Duration::from_millis(1000));
        let t = Instant::now();
        let mut other = connect(&front, &client);
        other.write_all(b"GET /app.js HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").unwrap();
        let mut answer = Vec::new();
        let _ = other.read_to_end(&mut answer);
        assert!(answer.starts_with(b"HTTP/1.1 200"), "no answer after {:?}", t.elapsed());
        assert!(t.elapsed() < STALL + Duration::from_secs(4), "answered after {:?}", t.elapsed());
        drop(greedy);
        stop.store(true, Ordering::SeqCst);
        front.shutdown();
    }

    /// A client that reads gets every pipelined answer, in order, also with large (chunked) and HEAD answers.
    #[test]
    fn pipelined_requests_of_a_reading_client_are_all_answered() {
        let (front, _http, stop, client) = test_server(300 * 1024);
        let mut c = connect(&front, &client);
        let mut req = String::new();
        for i in 0..12 {
            req.push_str(if i % 3 == 1 { "HEAD /a HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n" } else { GET });
        }
        req.push_str("GET /last HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        c.write_all(req.as_bytes()).unwrap();
        let mut all = Vec::new();
        let _ = c.read_to_end(&mut all);
        let mut a = Answers::default();
        a.waiting.extend((0..13).map(|i| i % 3 == 1 && i < 12));
        a.feed(&all);
        assert!(!a.lost);
        assert!(a.waiting.is_empty(), "{} answers missing of 13 ({} bytes)", a.waiting.len(), all.len());
        stop.store(true, Ordering::SeqCst);
        front.shutdown();
    }
}
