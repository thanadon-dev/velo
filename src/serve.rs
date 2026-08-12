use crate::epoll::{self, Epoll, Event};
use crate::http::{status_text, Ctype, Server, JSON, MAX_BODY, MAX_HEAD};
use crate::router::Fnv;
use crate::value::write_i64;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const READ_BUF: usize = 8 << 10;
const EVENTS: usize = 256;
const SWEEP: Duration = Duration::from_secs(1);
const MAX_PENDING: usize = 256 << 10;

struct Head {
    method: (usize, usize),
    path: (usize, usize),
    head_end: usize,
    content_len: usize,
    keep_alive: bool,
    head_only: bool,
    expects: bool,
    none_match: Option<u64>,
    error: Option<u16>,
}

fn parse_head(buf: &[u8], scanned: &mut usize) -> Option<Head> {
    let end = find_head_end(buf, scanned)?;
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
    let mut expects = false;
    let mut none_match = None;
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
        } else if eq_ignore_case(name, b"if-none-match") {
            none_match = parse_etag(val);
        } else if eq_ignore_case(name, b"expect") {
            expects = contains_ignore_case(val, b"100-continue");
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
        expects,
        none_match,
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
        expects: false,
        none_match: None,
        error: Some(code),
    }
}

fn find_head_end(buf: &[u8], scanned: &mut usize) -> Option<usize> {
    let mut i = scanned.saturating_sub(3);
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
    *scanned = buf.len();
    None
}

fn parse_etag(val: &[u8]) -> Option<u64> {
    let hex = val.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
    u64::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()
}

fn header_value<'a>(raw: &'a [u8], name: &str) -> Option<&'a str> {
    let mut pos = raw.iter().position(|&c| c == b'\n')? + 1;
    while pos < raw.len() {
        let nl = raw[pos..].iter().position(|&c| c == b'\n').map(|j| pos + j)?;
        let line = strip_cr(&raw[pos..nl]);
        pos = nl + 1;
        let Some(colon) = line.iter().position(|&c| c == b':') else { continue };
        if eq_ignore_case(&line[..colon], name.as_bytes()) {
            return std::str::from_utf8(trim(&line[colon + 1..])).ok();
        }
    }
    None
}

pub(crate) fn strip_cr(b: &[u8]) -> &[u8] {
    match b.last() {
        Some(b'\r') => &b[..b.len() - 1],
        _ => b,
    }
}

pub(crate) fn trim(b: &[u8]) -> &[u8] {
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
    write_head_tagged(out, status, ct, len, keep_alive, extra, None)
}

fn write_head_tagged(
    out: &mut Vec<u8>,
    status: u16,
    ct: Ctype,
    len: usize,
    keep_alive: bool,
    extra: &[u8],
    etag: Option<u64>,
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
    out.extend_from_slice(b"\r\nContent-Type: ");
    out.extend_from_slice(ct.as_bytes());
    out.extend_from_slice(b"\r\nContent-Length: ");
    write_i64(out, len as i64);
    out.extend_from_slice(if keep_alive {
        b"\r\nConnection: keep-alive\r\n".as_slice()
    } else {
        b"\r\nConnection: close\r\n".as_slice()
    });
    out.extend_from_slice(extra);
    if let Some(tag) = etag {
        out.extend_from_slice(b"ETag: \"");
        out.extend_from_slice(format!("{tag:x}").as_bytes());
        out.extend_from_slice(b"\"\r\n");
    }
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
    continued: bool,
    served: bool,
    scanned: usize,
    peer: String,
    opened: Instant,
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
            continued: false,
            served: false,
            scanned: 0,
            peer: String::new(),
            opened: Instant::now(),
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

fn pending(c: &Conn) -> usize {
    c.out.len() - c.wpos
}

fn process(srv: &Server, c: &mut Conn, headers: &[u8]) {
    loop {
        if pending(c) >= MAX_PENDING {
            c.compact();
            return;
        }
        let mut scanned = c.scanned;
        let parsed = parse_head(&c.inbuf[c.start..], &mut scanned);
        c.scanned = scanned;
        let Some(head) = parsed else {
            if c.inbuf.len() - c.start > MAX_HEAD {
                write_head(&mut c.out, 431, JSON, 0, false, headers);
                c.closing = true;
            }
            c.compact();
            return;
        };
        let need = head.head_end + head.content_len;
        if c.inbuf.len() - c.start < need {
            if head.expects && !c.continued && head.error.is_none() {
                c.out.extend_from_slice(b"HTTP/1.1 100 Continue\r\n\r\n");
                c.continued = true;
            }
            c.compact();
            return;
        }
        if let Some(code) = head.error {
            write_head(&mut c.out, code, JSON, 0, false, headers);
            c.closing = true;
            c.start += need;
            c.scanned = 0;
            c.compact();
            return;
        }
        let req = &c.inbuf[c.start..c.start + need];
        let method = std::str::from_utf8(&req[head.method.0..head.method.1]).unwrap_or("");
        let path = std::str::from_utf8(&req[head.path.0..head.path.1]).unwrap_or("/");
        if srv.rate > 0 {
            let key = match &srv.real_ip_header {
                Some(name) => header_value(&req[..head.head_end], name).unwrap_or(&c.peer),
                None => &c.peer,
            };
            if !srv.allow(key) {
                write_head(&mut c.out, 429, JSON, 0, head.keep_alive, headers);
                c.start += need;
                c.scanned = 0;
                c.served = true;
                if !head.keep_alive {
                    c.closing = true;
                    c.compact();
                    return;
                }
                continue;
            }
        }
        c.body.clear();
        let mut body = std::mem::take(&mut c.body);
        let started = srv.metrics_path.as_ref().map(|_| Instant::now());
        let (status, ct) =
            srv.handle(method, path, &req[head.head_end..], &req[..head.head_end], &mut body);
        if let Some(t0) = started {
            srv.record(t0.elapsed().as_micros() as u64, body.len());
        }
        if srv.log {
            eprintln!("{method} {path} {status} {}b", body.len());
        }
        let tag = if srv.etag && status == 200 && (method == "GET" || method == "HEAD") {
            Some(crate::http::etag_of(&body))
        } else {
            None
        };
        if tag.is_some() && tag == head.none_match {
            write_head_tagged(&mut c.out, 304, ct, 0, head.keep_alive, headers, tag);
        } else {
            write_head_tagged(&mut c.out, status, ct, body.len(), head.keep_alive, headers, tag);
            if !head.head_only && !empty_status(status) {
                c.out.extend_from_slice(&body);
            }
        }
        c.body = body;
        c.start += need;
        c.scanned = 0;
        c.served = true;
        c.continued = false;
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

pub(crate) fn worker(srv: &Server, listener: &TcpListener) -> std::io::Result<()> {
    let ep = Epoll::new()?;
    ep.add(listener, epoll::IN | epoll::EXCLUSIVE, u64::MAX)?;
    let mut events = vec![Event::default(); EVENTS];
    let mut conns: ConnMap = ConnMap::default();
    let mut dead: Vec<u64> = Vec::new();
    let mut last_sweep = Instant::now();
    let idle = Duration::from_secs(srv.keepalive_secs.max(1));
    let opening = Duration::from_secs(srv.header_secs.max(1));
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
                }
                loop {
                    if !c.inbuf.is_empty() && pending(c) < MAX_PENDING && !c.closing {
                        process(srv, c, &headers);
                    }
                    if pending(c) == 0 {
                        break;
                    }
                    let flushed = c.flush();
                    alive = flushed && alive;
                    if !flushed || pending(c) > 0 || c.inbuf.is_empty() || c.closing {
                        break;
                    }
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
                srv.conns.fetch_sub(1, Ordering::Relaxed);
            }
        }
        if last_sweep.elapsed() >= SWEEP {
            last_sweep = Instant::now();
            conns.retain(|_, c| {
                let alive =
                    if c.served { c.last.elapsed() < idle } else { c.opened.elapsed() < opening };
                if alive {
                    return true;
                }
                let _ = ep.remove(&c.stream);
                srv.conns.fetch_sub(1, Ordering::Relaxed);
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
        let peer = stream.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
        let mut conn = Conn::new(stream);
        conn.peer = peer;
        if ep.add(&conn.stream, epoll::IN | epoll::RDHUP, key).is_err() {
            continue;
        }
        conns.insert(key, conn);
        srv.conns.fetch_add(1, Ordering::Relaxed);
    }
}
