use crate::epoll::{self, Epoll, Event};
use crate::parser::{Ctx, Err_, Method, Program, Route};
use crate::router::{Fnv, Router};
use crate::store::Store;
use crate::value::{write_i64, Value};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const MAX_BODY: usize = 1 << 20;
pub const MAX_HEAD: usize = 8 << 10;
const READ_BUF: usize = 8 << 10;
const EVENTS: usize = 256;
const SWEEP: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctype {
    Json,
    Text,
}

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

extern "C" fn on_signal(_sig: i32) {
    SIGNALLED.store(true, Ordering::Relaxed);
}

pub fn install_signal_handlers() {
    for sig in [2, 15] {
        unsafe { signal(sig, on_signal as *const () as usize) };
    }
}

pub struct Server {
    pub routes: Vec<Route>,
    pub router: Router,
    pub store: Arc<Store>,
    pub max_conns: usize,
    pub keepalive_secs: u64,
    pub workers: usize,
    pub extra_headers: Vec<u8>,
    pub cors: bool,
    pub log: bool,
    stop: AtomicBool,
}

impl Server {
    pub fn new(prog: Program) -> Result<Arc<Server>, String> {
        let router = Router::build(&prog.routes)?;
        let cors = std::env::var("VELO_CORS").ok().filter(|v| !v.is_empty());
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        Ok(Arc::new(Server {
            routes: prog.routes,
            router,
            store: prog.store,
            max_conns: env_usize("VELO_MAX_CONNS", 65536),
            keepalive_secs: env_usize("VELO_KEEPALIVE", 60) as u64,
            workers: env_usize("VELO_WORKERS", cpus).max(1),
            extra_headers: cors_headers(&cors),
            cors: cors.is_some(),
            log: std::env::var("VELO_LOG").map(|v| v != "0").unwrap_or(false),
            stop: AtomicBool::new(false),
        }))
    }

    pub fn dispatch(
        &self,
        method: &str,
        path: &str,
        raw_body: &[u8],
        out: &mut Vec<u8>,
    ) -> (u16, Ctype) {
        self.handle(method, path, raw_body, &[], out)
    }

    pub fn handle(
        &self,
        method: &str,
        path: &str,
        raw_body: &[u8],
        raw_headers: &[u8],
        out: &mut Vec<u8>,
    ) -> (u16, Ctype) {
        let (path, query) = match path.find('?') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => (path, ""),
        };
        let Some(m) = Method::parse(method) else {
            return err_body(Err_ { status: 405, msg: "method not allowed" }, out);
        };
        let mut ctx = Ctx::default();
        let found = self.router.lookup(m, path, &mut ctx).or_else(|| {
            if m == Method::Head {
                self.router.lookup(Method::Get, path, &mut ctx)
            } else {
                None
            }
        });
        let Some(idx) = found else {
            if self.cors && m == Method::Options {
                return (204, Ctype::Json);
            }
            let e = if self.router.allows(path) {
                Err_ { status: 405, msg: "method not allowed" }
            } else {
                Err_ { status: 404, msg: "not found" }
            };
            return err_body(e, out);
        };
        let rt = &self.routes[idx];
        if rt.uses_query {
            ctx.query = crate::parser::parse_query(query);
        }
        if rt.uses_header {
            ctx.header = parse_headers(raw_headers);
        }
        if rt.uses_body && !raw_body.is_empty() {
            match crate::value::parse_json(raw_body) {
                Ok(v) => ctx.body = v,
                Err(_) => return err_body(crate::parser::BAD_BODY, out),
            }
        }
        if let Some(g) = &rt.guard {
            match g.eval(&ctx) {
                Ok(v) if crate::parser::truthy(&v) => {}
                Ok(_) => return err_body(Err_ { status: 401, msg: "unauthorized" }, out),
                Err(e) => return err_body(e, out),
            }
        }
        if let Some(k) = &rt.konst {
            out.extend_from_slice(k);
            return (rt.status, if rt.const_text { Ctype::Text } else { Ctype::Json });
        }
        match rt.expr.eval(&ctx) {
            Ok(Value::Str(s)) => {
                out.extend_from_slice(s.as_bytes());
                (rt.status, Ctype::Text)
            }
            Ok(v) => {
                v.write_json(out);
                (rt.status, Ctype::Json)
            }
            Err(e) => err_body(e, out),
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || SIGNALLED.load(Ordering::Relaxed)
    }

    pub fn listen(self: &Arc<Self>, addr: &str) -> std::io::Result<()> {
        self.serve(TcpListener::bind(addr)?)
    }

    pub fn serve(self: &Arc<Self>, listener: TcpListener) -> std::io::Result<()> {
        listener.set_nonblocking(true)?;
        let listener = Arc::new(listener);
        let mut handles = Vec::new();
        for _ in 1..self.workers {
            let (srv, lst) = (self.clone(), listener.clone());
            handles.push(std::thread::spawn(move || worker(&srv, &lst)));
        }
        let res = worker(self, &listener);
        for h in handles {
            let _ = h.join();
        }
        res
    }
}

pub fn parse_headers(raw: &[u8]) -> Value {
    let mut fields: Vec<(Arc<str>, Value)> = Vec::new();
    let mut pos = match raw.iter().position(|&c| c == b'\n') {
        Some(i) => i + 1,
        None => return Value::obj(fields),
    };
    while pos < raw.len() {
        let nl = match raw[pos..].iter().position(|&c| c == b'\n') {
            Some(j) => pos + j,
            None => raw.len(),
        };
        let line = strip_cr(&raw[pos..nl]);
        pos = nl + 1;
        let Some(colon) = line.iter().position(|&c| c == b':') else { continue };
        let name = std::str::from_utf8(&line[..colon])
            .unwrap_or("")
            .to_ascii_lowercase()
            .replace('-', "_");
        if name.is_empty() {
            continue;
        }
        let value = std::str::from_utf8(trim(&line[colon + 1..])).unwrap_or("");
        fields.push((Arc::from(name.as_str()), Value::str(value)));
    }
    Value::obj(fields)
}

fn cors_headers(origin: &Option<String>) -> Vec<u8> {
    let Some(origin) = origin else { return Vec::new() };
    let mut h = Vec::with_capacity(160);
    h.extend_from_slice(b"Access-Control-Allow-Origin: ");
    h.extend_from_slice(origin.as_bytes());
    h.extend_from_slice(
        b"\r\nAccess-Control-Allow-Methods: GET, POST, PUT, PATCH, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nAccess-Control-Max-Age: 86400\r\n",
    );
    h
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn err_body(e: Err_, out: &mut Vec<u8>) -> (u16, Ctype) {
    out.extend_from_slice(b"{\"error\":");
    crate::value::write_string(out, e.msg);
    out.push(b'}');
    (e.status, Ctype::Json)
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

struct Head {
    method: (usize, usize),
    path: (usize, usize),
    head_end: usize,
    content_len: usize,
    keep_alive: bool,
    head_only: bool,
    error: Option<u16>,
}

fn parse_head(buf: &[u8]) -> Option<Head> {
    let end = find_head_end(buf)?;
    let line_end = buf.iter().position(|&c| c == b'\n')?;
    let line = strip_cr(&buf[..line_end]);
    let mut parts = line.splitn(3, |&c| c == b' ');
    let (Some(m), Some(p), Some(v)) = (parts.next(), parts.next(), parts.next()) else {
        return Some(bad(end, 400));
    };
    let method = (0, m.len());
    let path = (m.len() + 1, m.len() + 1 + p.len());
    let http11 = v.starts_with(b"HTTP/1.1");
    let mut keep_alive = http11;
    let mut content_len = 0usize;
    let mut seen_len = false;
    let mut chunked = false;
    let mut pos = line_end + 1;
    while pos < end {
        let nl = match buf[pos..end].iter().position(|&c| c == b'\n') {
            Some(j) => pos + j,
            None => break,
        };
        let h = strip_cr(&buf[pos..nl]);
        pos = nl + 1;
        let Some(colon) = h.iter().position(|&c| c == b':') else { continue };
        let name = &h[..colon];
        let val = trim(&h[colon + 1..]);
        if eq_ignore_case(name, b"content-length") {
            match std::str::from_utf8(val).ok().and_then(|s| s.parse::<usize>().ok()) {
                Some(n) if !seen_len || n == content_len => {
                    content_len = n;
                    seen_len = true;
                }
                _ => return Some(bad(end, 400)),
            }
        } else if eq_ignore_case(name, b"connection") {
            if contains_ignore_case(val, b"close") {
                keep_alive = false;
            } else if contains_ignore_case(val, b"keep-alive") {
                keep_alive = true;
            }
        } else if eq_ignore_case(name, b"transfer-encoding")
            && contains_ignore_case(val, b"chunked")
        {
            chunked = true;
        }
    }
    let head_only = m == b"HEAD";
    let error = if chunked {
        Some(411)
    } else if content_len > MAX_BODY {
        Some(413)
    } else {
        None
    };
    Some(Head {
        method,
        path,
        head_end: end,
        content_len: if error.is_some() { 0 } else { content_len },
        keep_alive: keep_alive && error.is_none(),
        head_only,
        error,
    })
}

fn bad(end: usize, code: u16) -> Head {
    Head {
        method: (0, 0),
        path: (0, 0),
        head_end: end,
        content_len: 0,
        keep_alive: false,
        head_only: false,
        error: Some(code),
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
        if i + 3 < buf.len()
            && buf[i] == b'\r'
            && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            return Some(i + 4);
        }
        i += 1;
    }
    None
}

fn strip_cr(b: &[u8]) -> &[u8] {
    match b.last() {
        Some(b'\r') => &b[..b.len() - 1],
        _ => b,
    }
}

fn trim(b: &[u8]) -> &[u8] {
    let mut s = 0;
    let mut e = b.len();
    while s < e && (b[s] == b' ' || b[s] == b'\t') {
        s += 1;
    }
    while e > s && (b[e - 1] == b' ' || b[e - 1] == b'\t' || b[e - 1] == b'\r') {
        e -= 1;
    }
    &b[s..e]
}

fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_ascii_lowercase() == *y)
}

fn contains_ignore_case(hay: &[u8], needle: &[u8]) -> bool {
    if needle.len() > hay.len() {
        return false;
    }
    (0..=hay.len() - needle.len()).any(|i| eq_ignore_case(&hay[i..i + needle.len()], needle))
}

fn empty_status(status: u16) -> bool {
    status == 204 || status == 304
}

fn write_head(
    out: &mut Vec<u8>,
    status: u16,
    ct: Ctype,
    len: usize,
    keep_alive: bool,
    extra: &[u8],
) {
    out.extend_from_slice(b"HTTP/1.1 ");
    write_i64(out, status as i64);
    out.push(b' ');
    out.extend_from_slice(status_text(status).as_bytes());
    if empty_status(status) {
        out.extend_from_slice(if keep_alive {
            b"\r\nConnection: keep-alive\r\n".as_slice()
        } else {
            b"\r\nConnection: close\r\n".as_slice()
        });
        out.extend_from_slice(extra);
        out.extend_from_slice(b"\r\n");
        return;
    }
    out.extend_from_slice(match ct {
        Ctype::Json => b"\r\nContent-Type: application/json\r\nContent-Length: ".as_slice(),
        Ctype::Text => {
            b"\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: ".as_slice()
        }
    });
    write_i64(out, len as i64);
    out.extend_from_slice(if keep_alive {
        b"\r\nConnection: keep-alive\r\n".as_slice()
    } else {
        b"\r\nConnection: close\r\n".as_slice()
    });
    out.extend_from_slice(extra);
    out.extend_from_slice(b"\r\n");
}

struct Conn {
    stream: TcpStream,
    inbuf: Vec<u8>,
    start: usize,
    out: Vec<u8>,
    wpos: usize,
    body: Vec<u8>,
    closing: bool,
    want_out: bool,
    last: Instant,
}

impl Conn {
    fn new(stream: TcpStream) -> Conn {
        Conn {
            stream,
            inbuf: Vec::with_capacity(READ_BUF),
            start: 0,
            out: Vec::with_capacity(READ_BUF),
            wpos: 0,
            body: Vec::with_capacity(512),
            closing: false,
            want_out: false,
            last: Instant::now(),
        }
    }

    fn compact(&mut self) {
        if self.start == 0 {
            return;
        }
        if self.start >= self.inbuf.len() {
            self.inbuf.clear();
        } else {
            self.inbuf.drain(..self.start);
        }
        self.start = 0;
    }

    fn fill(&mut self) -> bool {
        loop {
            if self.inbuf.len() - self.start > MAX_HEAD + MAX_BODY {
                return false;
            }
            let len = self.inbuf.len();
            self.inbuf.resize(len + READ_BUF, 0);
            match self.stream.read(&mut self.inbuf[len..]) {
                Ok(0) => {
                    self.inbuf.truncate(len);
                    return false;
                }
                Ok(n) => {
                    self.inbuf.truncate(len + n);
                    if n < READ_BUF {
                        return true;
                    }
                }
                Err(e) => {
                    self.inbuf.truncate(len);
                    return matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted);
                }
            }
        }
    }

    fn flush(&mut self) -> bool {
        while self.wpos < self.out.len() {
            match self.stream.write(&self.out[self.wpos..]) {
                Ok(0) => return false,
                Ok(n) => self.wpos += n,
                Err(e) => {
                    return matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted)
                }
            }
        }
        self.out.clear();
        self.wpos = 0;
        true
    }
}

fn process(srv: &Server, c: &mut Conn, headers: &[u8]) {
    loop {
        let Some(head) = parse_head(&c.inbuf[c.start..]) else {
            if c.inbuf.len() - c.start > MAX_HEAD {
                write_head(&mut c.out, 431, Ctype::Json, 0, false, headers);
                c.closing = true;
            }
            c.compact();
            return;
        };
        let need = head.head_end + head.content_len;
        if c.inbuf.len() - c.start < need {
            c.compact();
            return;
        }
        if let Some(code) = head.error {
            write_head(&mut c.out, code, Ctype::Json, 0, false, headers);
            c.closing = true;
            c.start += need;
            c.compact();
            return;
        }
        let req = &c.inbuf[c.start..c.start + need];
        let method = std::str::from_utf8(&req[head.method.0..head.method.1]).unwrap_or("");
        let path = std::str::from_utf8(&req[head.path.0..head.path.1]).unwrap_or("/");
        c.body.clear();
        let mut body = std::mem::take(&mut c.body);
        let (status, ct) =
            srv.handle(method, path, &req[head.head_end..], &req[..head.head_end], &mut body);
        if srv.log {
            eprintln!("{method} {path} {status} {}b", body.len());
        }
        write_head(&mut c.out, status, ct, body.len(), head.keep_alive, headers);
        if !head.head_only && !empty_status(status) {
            c.out.extend_from_slice(&body);
        }
        c.body = body;
        c.start += need;
        if !head.keep_alive {
            c.closing = true;
            c.compact();
            return;
        }
        if c.start >= c.inbuf.len() {
            c.compact();
            return;
        }
    }
}

type ConnMap = HashMap<u64, Conn, BuildHasherDefault<Fnv>>;

fn worker(srv: &Server, listener: &TcpListener) -> std::io::Result<()> {
    let ep = Epoll::new()?;
    ep.add(listener, epoll::IN | epoll::EXCLUSIVE, u64::MAX)?;
    let mut events = vec![Event::default(); EVENTS];
    let mut conns: ConnMap = ConnMap::default();
    let mut dead: Vec<u64> = Vec::new();
    let mut last_sweep = Instant::now();
    let idle = Duration::from_secs(srv.keepalive_secs.max(1));
    let mut headers = response_headers(srv);
    let mut header_age = Instant::now();

    while !srv.stopping() {
        let n = ep.wait(&mut events, 250)?;
        if header_age.elapsed() >= Duration::from_secs(1) {
            headers = response_headers(srv);
            header_age = Instant::now();
        }
        for ev in &events[..n] {
            let (flags, key) = (ev.events, ev.data);
            if key == u64::MAX {
                accept_all(srv, listener, &ep, &mut conns);
                continue;
            }
            let Some(c) = conns.get_mut(&key) else { continue };
            c.last = Instant::now();
            let mut alive = true;
            if flags & (epoll::ERR | epoll::HUP) != 0 {
                alive = false;
            } else {
                if flags & (epoll::IN | epoll::RDHUP) != 0 {
                    alive = c.fill();
                    if !c.inbuf.is_empty() {
                        process(srv, c, &headers);
                    }
                }
                if alive || !c.out.is_empty() {
                    alive = c.flush() && alive;
                }
                if alive && c.closing && c.wpos >= c.out.len() {
                    alive = false;
                }
                if alive {
                    let want_out = c.wpos < c.out.len();
                    if want_out != c.want_out {
                        c.want_out = want_out;
                        let mut mask = epoll::IN | epoll::RDHUP;
                        if want_out {
                            mask |= epoll::OUT;
                        }
                        if ep.modify(&c.stream, mask, key).is_err() {
                            alive = false;
                        }
                    }
                }
            }
            if !alive {
                dead.push(key);
            }
        }
        for key in dead.drain(..) {
            if let Some(c) = conns.remove(&key) {
                let _ = ep.remove(&c.stream);
            }
        }
        if last_sweep.elapsed() >= SWEEP {
            last_sweep = Instant::now();
            conns.retain(|_, c| {
                if c.last.elapsed() < idle {
                    return true;
                }
                let _ = ep.remove(&c.stream);
                false
            });
        }
    }
    Ok(())
}

fn response_headers(srv: &Server) -> Vec<u8> {
    let mut h = crate::date::header(std::time::SystemTime::now());
    h.extend_from_slice(&srv.extra_headers);
    h
}

fn accept_all(srv: &Server, listener: &TcpListener, ep: &Epoll, conns: &mut ConnMap) {
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
                return;
            }
        };
        if conns.len() >= srv.max_conns {
            let _ = (&stream).write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            continue;
        }
        if stream.set_nonblocking(true).is_err() {
            continue;
        }
        let _ = stream.set_nodelay(true);
        let key = stream.as_raw_fd() as u64;
        let conn = Conn::new(stream);
        if ep.add(&conn.stream, epoll::IN | epoll::RDHUP, key).is_err() {
            continue;
        }
        conns.insert(key, conn);
    }
}
