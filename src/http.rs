use crate::parser::{Ctx, Err_, Method, Program, Route};
use crate::router::{Fnv, Router};
use crate::socket::Listener;
use crate::store::Store;
use crate::value::{write_i64, Value};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MAX_BODY: usize = 1 << 20;
const RATE_SHARDS: usize = 16;
type RateShard = HashMap<String, (Instant, u32), BuildHasherDefault<Fnv>>;
const RATE_KEYS_MAX: usize = 4096;
pub const MAX_HEAD: usize = 8 << 10;

pub type Ctype = &'static str;

pub const JSON: Ctype = "application/json";
pub const TEXT: Ctype = "text/plain; charset=utf-8";

pub fn ctype_for(path: &str) -> Ctype {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => JSON,
        "svg" => "image/svg+xml",
        "xml" => "application/xml",
        "csv" => "text/csv; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        _ => TEXT,
    }
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
    pub header_secs: u64,
    pub workers: usize,
    pub extra_headers: Vec<u8>,
    pub cors: bool,
    pub log: bool,
    pub log_json: bool,
    pub metrics_path: Option<String>,
    pub etag: bool,
    pub rate: u32,
    pub real_ip_header: Option<String>,
    limiter: Vec<Mutex<RateShard>>,
    started: Instant,
    requests: AtomicU64,
    failures: AtomicU64,
    micros: AtomicU64,
    max_micros: AtomicU64,
    bytes_out: AtomicU64,
    pub(crate) conns: AtomicU64,
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
            header_secs: env_usize("VELO_HEADER_TIMEOUT", 10) as u64,
            workers: env_usize("VELO_WORKERS", cpus).max(1),
            extra_headers: {
                let mut h = cors_headers(&cors);
                h.extend_from_slice(&extra_headers(std::env::var("VELO_HEADERS").ok()));
                h
            },
            cors: cors.is_some(),
            log: std::env::var("VELO_LOG").map(|v| v != "0").unwrap_or(false),
            log_json: std::env::var("VELO_LOG").map(|v| v == "json").unwrap_or(false),
            metrics_path: std::env::var("VELO_METRICS").ok().filter(|v| v.starts_with('/')),
            etag: std::env::var("VELO_ETAG").map(|v| v != "0").unwrap_or(false),
            rate: env_usize("VELO_RATE", 0) as u32,
            real_ip_header: std::env::var("VELO_REAL_IP_HEADER")
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| v.to_ascii_lowercase()),
            limiter: (0..RATE_SHARDS).map(|_| Mutex::new(HashMap::default())).collect(),
            started: Instant::now(),
            requests: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            micros: AtomicU64::new(0),
            max_micros: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            conns: AtomicU64::new(0),
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
        self.requests.fetch_add(1, Ordering::Relaxed);
        if self.metrics_path.as_deref() == Some(path) {
            self.write_metrics(out);
            return (200, JSON);
        }
        let Some(m) = Method::parse(method) else {
            return self.fail(Err_ { status: 405, msg: "method not allowed" }, out);
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
                return (204, JSON);
            }
            let e = if self.router.allows(path) {
                Err_ { status: 405, msg: "method not allowed" }
            } else {
                Err_ { status: 404, msg: "not found" }
            };
            return self.fail(e, out);
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
                Err(_) => match form_body(raw_body) {
                    Some(v) => ctx.body = v,
                    None => return self.fail(crate::parser::BAD_BODY, out),
                },
            }
        }
        if let Some(g) = &rt.guard {
            match g.eval(&ctx) {
                Ok(v) if crate::parser::truthy(&v) => {}
                Ok(_) => {
                    let msg = if rt.guard_status == 400 { "invalid body" } else { "unauthorized" };
                    return self.fail(Err_ { status: rt.guard_status, msg }, out);
                }
                Err(e) => return self.fail(e, out),
            }
        }
        if let Some(k) = &rt.konst {
            out.extend_from_slice(k);
            return (rt.status, rt.const_ctype);
        }
        if rt.expr.renders_json() {
            let mark = out.len();
            return match rt.expr.write_json(&ctx, out) {
                Ok(()) => (rt.status, JSON),
                Err(e) => {
                    out.truncate(mark);
                    self.fail(e, out)
                }
            };
        }
        match rt.expr.eval(&ctx) {
            Ok(Value::Str(s)) => {
                out.extend_from_slice(s.as_bytes());
                (rt.status, TEXT)
            }
            Ok(v) => {
                v.write_json(out);
                (rt.status, JSON)
            }
            Err(e) => self.fail(e, out),
        }
    }

    fn write_metrics(&self, out: &mut Vec<u8>) {
        let f = |out: &mut Vec<u8>, key: &str, n: u64| {
            out.extend_from_slice(key.as_bytes());
            write_i64(out, n as i64);
        };
        out.extend_from_slice(b"{\"version\":\"");
        out.extend_from_slice(crate::VERSION.as_bytes());
        out.push(b'"');
        f(out, ",\"uptime_ms\":", self.started.elapsed().as_millis() as u64);
        f(out, ",\"requests\":", self.requests.load(Ordering::Relaxed));
        f(out, ",\"failures\":", self.failures.load(Ordering::Relaxed));
        f(out, ",\"connections\":", self.conns.load(Ordering::Relaxed));
        let requests = self.requests.load(Ordering::Relaxed).max(1);
        f(out, ",\"bytes_out\":", self.bytes_out.load(Ordering::Relaxed));
        f(out, ",\"avg_micros\":", self.micros.load(Ordering::Relaxed) / requests);
        f(out, ",\"max_micros\":", self.max_micros.load(Ordering::Relaxed));
        f(out, ",\"routes\":", self.routes.len() as u64);
        f(out, ",\"workers\":", self.workers as u64);
        out.push(b'}');
    }

    pub fn log_line(&self, method: &str, path: &str, status: u16, bytes: usize, micros: u64) {
        let mut out = Vec::with_capacity(96);
        if self.log_json {
            out.extend_from_slice(b"{\"method\":");
            crate::value::write_string(&mut out, method);
            out.extend_from_slice(b",\"path\":");
            crate::value::write_string(&mut out, path);
            out.extend_from_slice(b",\"status\":");
            write_i64(&mut out, status as i64);
            out.extend_from_slice(b",\"bytes\":");
            write_i64(&mut out, bytes as i64);
            out.extend_from_slice(b",\"micros\":");
            write_i64(&mut out, micros as i64);
            out.push(b'}');
        } else {
            out.extend_from_slice(method.as_bytes());
            out.push(b' ');
            out.extend_from_slice(path.as_bytes());
            out.push(b' ');
            write_i64(&mut out, status as i64);
            out.push(b' ');
            write_i64(&mut out, bytes as i64);
            out.extend_from_slice(b"b ");
            write_i64(&mut out, micros as i64);
            out.extend_from_slice(b"us");
        }
        out.push(b'\n');
        let _ = std::io::Write::write_all(&mut std::io::stderr(), &out);
    }

    pub fn record(&self, micros: u64, bytes: usize) {
        self.micros.fetch_add(micros, Ordering::Relaxed);
        self.bytes_out.fetch_add(bytes as u64, Ordering::Relaxed);
        self.max_micros.fetch_max(micros, Ordering::Relaxed);
    }

    pub fn allow(&self, key: &str) -> bool {
        if self.rate == 0 {
            return true;
        }
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in key.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let mut shard = self.limiter[hash as usize % RATE_SHARDS].lock().unwrap();
        let now = Instant::now();
        if shard.len() > RATE_KEYS_MAX {
            shard.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(1));
        }
        if let Some(slot) = shard.get_mut(key) {
            if now.duration_since(slot.0) >= Duration::from_secs(1) {
                *slot = (now, 0);
            }
            slot.1 += 1;
            return slot.1 <= self.rate;
        }
        shard.insert(key.to_string(), (now, 1));
        true
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || SIGNALLED.load(Ordering::Relaxed)
    }

    pub fn listen(self: &Arc<Self>, addr: &str) -> std::io::Result<()> {
        self.serve(Listener::bind(addr)?)
    }

    pub fn serve(self: &Arc<Self>, listener: Listener) -> std::io::Result<()> {
        listener.set_nonblocking(true)?;
        let listener = Arc::new(listener);
        let mut handles = Vec::new();
        for _ in 1..self.workers {
            let (srv, lst) = (self.clone(), listener.clone());
            handles.push(std::thread::spawn(move || crate::serve::worker(&srv, &lst)));
        }
        let res = crate::serve::worker(self, &listener);
        for h in handles {
            let _ = h.join();
        }
        res
    }
}

pub fn form_body(raw: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(raw).ok()?;
    if !text.contains('=') || text.starts_with('{') || text.starts_with('[') {
        return None;
    }
    if text.split('&').any(|pair| pair.is_empty() || pair.starts_with('=')) {
        return None;
    }
    Some(crate::parser::parse_query(text))
}

pub fn etag_of(body: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in body {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h ^ (body.len() as u64)
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
        let line = crate::serve::strip_cr(&raw[pos..nl]);
        pos = nl + 1;
        let Some(colon) = line.iter().position(|&c| c == b':') else { continue };
        let name = std::str::from_utf8(&line[..colon])
            .unwrap_or("")
            .to_ascii_lowercase()
            .replace('-', "_");
        if name.is_empty() {
            continue;
        }
        let value = std::str::from_utf8(crate::serve::trim(&line[colon + 1..])).unwrap_or("");
        fields.push((Arc::from(name.as_str()), Value::str(value)));
    }
    Value::obj(fields)
}

pub fn extra_headers(spec: Option<String>) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(spec) = spec else { return out };
    for line in spec.split(['\n', ';']) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            eprintln!("velo: ignoring header without a colon: {line:?}");
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || name.contains(|c: char| c.is_control() || c == ' ') {
            eprintln!("velo: ignoring bad header name: {name:?}");
            continue;
        }
        if value.contains(|c: char| c.is_control()) {
            eprintln!("velo: ignoring bad header value for {name:?}");
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
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

impl Server {
    fn fail(&self, e: Err_, out: &mut Vec<u8>) -> (u16, Ctype) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        err_body(e, out)
    }
}

fn err_body(e: Err_, out: &mut Vec<u8>) -> (u16, Ctype) {
    out.extend_from_slice(b"{\"error\":");
    crate::value::write_string(out, e.msg);
    out.push(b'}');
    (e.status, JSON)
}

pub(crate) fn status_text(code: u16) -> &'static str {
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
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}
