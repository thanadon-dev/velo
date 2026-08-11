use crate::parser::{Ctx, Err_, Method, Program, Route};
use crate::router::Router;
use crate::store::Store;
use crate::value::{write_i64, Value};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_BODY: usize = 1 << 20;
pub const MAX_HEAD: usize = 8 << 10;
const READ_BUF: usize = 8 << 10;
const STACK_SIZE: usize = 128 << 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctype {
    Json,
    Text,
}

pub struct Server {
    pub routes: Vec<Route>,
    pub router: Router,
    pub store: Arc<Store>,
    pub max_conns: usize,
    pub keepalive_secs: u64,
    conns: AtomicUsize,
}

impl Server {
    pub fn new(prog: Program) -> Result<Arc<Server>, String> {
        let router = Router::build(&prog.routes)?;
        Ok(Arc::new(Server {
            routes: prog.routes,
            router,
            store: prog.store,
            max_conns: env_usize("VELO_MAX_CONNS", 4096),
            keepalive_secs: env_usize("VELO_KEEPALIVE", 60) as u64,
            conns: AtomicUsize::new(0),
        }))
    }

    pub fn dispatch(
        &self,
        method: &str,
        path: &str,
        raw_body: &[u8],
        out: &mut Vec<u8>,
    ) -> (u16, Ctype) {
        let path = match path.find('?') {
            Some(i) => &path[..i],
            None => path,
        };
        let Some(m) = Method::from_str(method) else {
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
            let e = if self.router.allows(path) {
                Err_ { status: 405, msg: "method not allowed" }
            } else {
                Err_ { status: 404, msg: "not found" }
            };
            return err_body(e, out);
        };
        let rt = &self.routes[idx];
        if rt.uses_body && !raw_body.is_empty() {
            match crate::value::parse_json(raw_body) {
                Ok(v) => ctx.body = v,
                Err(_) => return err_body(crate::parser::BAD_BODY, out),
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

    pub fn listen(self: &Arc<Self>, addr: &str) -> std::io::Result<()> {
        self.serve(TcpListener::bind(addr)?)
    }

    pub fn serve(self: &Arc<Self>, listener: TcpListener) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if self.conns.load(Ordering::Relaxed) >= self.max_conns {
                let _ = (&stream).write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
            self.conns.fetch_add(1, Ordering::Relaxed);
            let srv = self.clone();
            let spawned = std::thread::Builder::new()
                .stack_size(STACK_SIZE)
                .spawn(move || {
                    serve_conn(&srv, stream);
                    srv.conns.fetch_sub(1, Ordering::Relaxed);
                });
            if spawned.is_err() {
                self.conns.fetch_sub(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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
        204 => "No Content",
        400 => "Bad Request",
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
                Some(n) => content_len = n,
                None => return Some(bad(end, 400)),
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
    if chunked {
        return Some(Head { method, path, head_end: end, content_len: 0, keep_alive: false, head_only, error: Some(411) });
    }
    if content_len > MAX_BODY {
        return Some(Head { method, path, head_end: end, content_len, keep_alive: false, head_only, error: Some(413) });
    }
    Some(Head { method, path, head_end: end, content_len, keep_alive, head_only, error: None })
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
        if i + 3 < buf.len() && buf[i] == b'\r' && buf[i + 1] == b'\n' && buf[i + 2] == b'\r' && buf[i + 3] == b'\n' {
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

fn write_head(out: &mut Vec<u8>, status: u16, ct: Ctype, len: usize, keep_alive: bool) {
    out.extend_from_slice(b"HTTP/1.1 ");
    write_i64(out, status as i64);
    out.push(b' ');
    out.extend_from_slice(status_text(status).as_bytes());
    out.extend_from_slice(match ct {
        Ctype::Json => b"\r\nContent-Type: application/json\r\nContent-Length: ".as_slice(),
        Ctype::Text => b"\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: ".as_slice(),
    });
    write_i64(out, len as i64);
    if keep_alive {
        out.extend_from_slice(b"\r\nConnection: keep-alive\r\n\r\n");
    } else {
        out.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    }
}

fn serve_conn(srv: &Server, mut stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let ka = Duration::from_secs(srv.keepalive_secs.max(1));
    let _ = stream.set_read_timeout(Some(ka));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

    let mut buf: Vec<u8> = Vec::with_capacity(READ_BUF);
    let mut out: Vec<u8> = Vec::with_capacity(READ_BUF);
    let mut body: Vec<u8> = Vec::with_capacity(1024);
    let mut start = 0usize;

    loop {
        let head = match parse_head(&buf[start..]) {
            Some(h) => h,
            None => {
                if buf.len() - start > MAX_HEAD {
                    write_head(&mut out, 431, Ctype::Json, 0, false);
                    let _ = stream.write_all(&out);
                    return;
                }
                if !out.is_empty() {
                    if stream.write_all(&out).is_err() {
                        return;
                    }
                    out.clear();
                }
                if start > 0 {
                    buf.drain(..start);
                    start = 0;
                }
                match read_more(&mut stream, &mut buf) {
                    Some(true) => continue,
                    _ => return,
                }
            }
        };
        let need = head.head_end + head.content_len;
        if buf.len() - start < need {
            if head.content_len > MAX_BODY {
                write_head(&mut out, 413, Ctype::Json, 0, false);
                let _ = stream.write_all(&out);
                return;
            }
            if !out.is_empty() {
                if stream.write_all(&out).is_err() {
                    return;
                }
                out.clear();
            }
            if start > 0 {
                buf.drain(..start);
                start = 0;
            }
            match read_more(&mut stream, &mut buf) {
                Some(true) => continue,
                _ => return,
            }
        }

        let req = &buf[start..start + need];
        let keep_alive = head.keep_alive;
        if let Some(code) = head.error {
            write_head(&mut out, code, Ctype::Json, 0, false);
            let _ = stream.write_all(&out);
            return;
        }
        let method = std::str::from_utf8(&req[head.method.0..head.method.1]).unwrap_or("");
        let path = std::str::from_utf8(&req[head.path.0..head.path.1]).unwrap_or("/");
        body.clear();
        let (status, ct) = srv.dispatch(method, path, &req[head.head_end..], &mut body);
        write_head(&mut out, status, ct, body.len(), keep_alive);
        if !head.head_only {
            out.extend_from_slice(&body);
        }
        start += need;

        if !keep_alive {
            let _ = stream.write_all(&out);
            return;
        }
        if start >= buf.len() || out.len() >= READ_BUF {
            if stream.write_all(&out).is_err() {
                return;
            }
            out.clear();
        }
        if start >= buf.len() {
            buf.clear();
            start = 0;
            match read_more(&mut stream, &mut buf) {
                Some(true) => continue,
                _ => return,
            }
        }
    }
}

fn read_more(stream: &mut TcpStream, buf: &mut Vec<u8>) -> Option<bool> {
    let len = buf.len();
    if buf.capacity() - len < READ_BUF {
        buf.reserve(READ_BUF);
    }
    buf.resize(len + READ_BUF, 0);
    let res = stream.read(&mut buf[len..]);
    match res {
        Ok(0) => {
            buf.truncate(len);
            None
        }
        Ok(n) => {
            buf.truncate(len + n);
            Some(true)
        }
        Err(e) if e.kind() == ErrorKind::Interrupted => {
            buf.truncate(len);
            Some(true)
        }
        Err(_) => {
            buf.truncate(len);
            None
        }
    }
}
