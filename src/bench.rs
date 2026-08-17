use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::socket::Stream;

pub struct Args {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub method: String,
    pub body: String,
    pub conns: usize,
    pub unix: Option<String>,
    pub secs: u64,
    pub pipeline: usize,
    pub headers: Vec<String>,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            host: "127.0.0.1".into(),
            port: 8080,
            path: "/".into(),
            method: "GET".into(),
            body: String::new(),
            conns: 50,
            unix: None,
            secs: 5,
            pipeline: 1,
            headers: Vec::new(),
        }
    }
}

pub struct Report {
    pub requests: u64,
    pub errors: u64,
    pub refused: u64,
    pub bytes: u64,
    pub elapsed: f64,
    pub p50: f64,
    pub p99: f64,
    pub max: f64,
}

impl Report {
    pub fn per_second(&self) -> f64 {
        if self.elapsed <= 0.0 {
            return 0.0;
        }
        self.requests as f64 / self.elapsed
    }
}

pub fn run(a: Args) -> Report {
    let a = Arc::new(a);
    let req = Arc::new(build_request(&a));
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let refused = Arc::new(AtomicU64::new(0));

    let start = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..a.conns {
        let (a, req, stop, done, errors, bytes, refused) = (
            a.clone(),
            req.clone(),
            stop.clone(),
            done.clone(),
            errors.clone(),
            bytes.clone(),
            refused.clone(),
        );
        handles
            .push(std::thread::spawn(move || worker(a, req, stop, done, errors, bytes, refused)));
    }
    std::thread::sleep(Duration::from_secs(a.secs));
    stop.store(true, Ordering::Relaxed);
    let mut lat: Vec<u64> = Vec::new();
    for h in handles {
        lat.extend(h.join().unwrap_or_default());
    }
    let elapsed = start.elapsed().as_secs_f64();
    lat.sort_unstable();
    let pick = |q: f64| -> f64 {
        if lat.is_empty() {
            return 0.0;
        }
        lat[((lat.len() as f64 * q) as usize).min(lat.len() - 1)] as f64 / 1000.0
    };
    Report {
        requests: done.load(Ordering::Relaxed),
        errors: errors.load(Ordering::Relaxed),
        refused: refused.load(Ordering::Relaxed),
        bytes: bytes.load(Ordering::Relaxed),
        elapsed,
        p50: pick(0.50),
        p99: pick(0.99),
        max: pick(1.0),
    }
}

fn refused(head: &[u8]) -> bool {
    let Some(code) = head.split(|b| *b == b' ').nth(1) else { return false };
    let code = std::str::from_utf8(code).unwrap_or("");
    code.parse::<u16>().is_ok_and(|status| status >= 400)
}

fn build_request(a: &Args) -> Vec<u8> {
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: keep-alive\r\n",
        a.method, a.path, a.host, a.port
    );
    for header in &a.headers {
        req.push_str(header.trim());
        req.push_str("\r\n");
    }
    if !a.body.is_empty() {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            a.body.len(),
            a.body
        ));
    } else {
        req.push_str("\r\n");
    }
    req.into_bytes()
}

fn worker(
    a: Arc<Args>,
    req: Arc<Vec<u8>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    refused: Arc<AtomicU64>,
) -> Vec<u64> {
    let mut lat = Vec::with_capacity(1 << 16);
    let mut at = 0usize;
    let target = match &a.unix {
        Some(path) => path.clone(),
        None => format!("{}:{}", a.host, a.port),
    };
    let Ok(mut s) = Stream::connect(&target) else {
        errors.fetch_add(1, Ordering::Relaxed);
        return lat;
    };
    s.set_nodelay();
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut batch = Vec::with_capacity(req.len() * a.pipeline);
    for _ in 0..a.pipeline {
        batch.extend_from_slice(&req);
    }
    let mut buf = vec![0u8; 64 << 10];
    let mut counter = Counter::default();
    let mut local_done = 0u64;
    let mut local_bytes = 0u64;
    let mut closed = false;
    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        if s.write_all(&batch).is_err() {
            if !counter.at_boundary() {
                errors.fetch_add(1, Ordering::Relaxed);
            }
            break;
        }
        let mut seen = 0usize;
        while seen < a.pipeline {
            match s.read(&mut buf) {
                Ok(0) => {
                    if !counter.at_boundary() {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                    closed = true;
                    break;
                }
                Ok(n) => {
                    local_bytes += n as u64;
                    seen += counter.feed(&buf[..n]);
                }
                Err(e) => {
                    let ended = matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    );
                    if !(ended && counter.at_boundary()) {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                    closed = true;
                    break;
                }
            }
        }
        if seen == 0 || closed {
            break;
        }
        let us = t0.elapsed().as_micros() as u64 / a.pipeline as u64;
        if lat.len() < lat.capacity() {
            lat.push(us);
        } else {
            lat[at] = us;
            at = (at + 1) % lat.len();
        }
        local_done += seen as u64;
    }
    done.fetch_add(local_done, Ordering::Relaxed);
    bytes.fetch_add(local_bytes, Ordering::Relaxed);
    refused.fetch_add(counter.refused, Ordering::Relaxed);
    lat
}

#[derive(Default)]
struct Counter {
    head: Vec<u8>,
    body_left: usize,
    refused: u64,
}

impl Counter {
    fn at_boundary(&self) -> bool {
        self.head.is_empty() && self.body_left == 0
    }
}

impl Counter {
    fn feed(&mut self, mut chunk: &[u8]) -> usize {
        let mut done = 0;
        while !chunk.is_empty() {
            if self.body_left > 0 {
                let take = self.body_left.min(chunk.len());
                self.body_left -= take;
                chunk = &chunk[take..];
                if self.body_left == 0 {
                    done += 1;
                }
                continue;
            }
            if self.head.is_empty() {
                let Some(end) = find(chunk, b"\r\n\r\n") else {
                    self.head.extend_from_slice(chunk);
                    return done;
                };
                let len = content_length(&chunk[..end]);
                if refused(&chunk[..end]) {
                    self.refused += 1;
                }
                chunk = &chunk[end + 4..];
                if len == 0 {
                    done += 1;
                } else {
                    self.body_left = len;
                }
                continue;
            }
            self.head.extend_from_slice(chunk);
            let Some(end) = find(&self.head, b"\r\n\r\n") else {
                return done;
            };
            let consumed = chunk.len() - (self.head.len() - (end + 4));
            chunk = &chunk[consumed..];
            let len = content_length(&self.head[..end]);
            if refused(&self.head[..end]) {
                self.refused += 1;
            }
            self.head.clear();
            if len == 0 {
                done += 1;
            } else {
                self.body_left = len;
            }
        }
        done
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if hay.len() < needle.len() {
        return None;
    }
    let last = needle[needle.len() - 1];
    let mut i = needle.len() - 1;
    while i < hay.len() {
        if hay[i] == last && &hay[i + 1 - needle.len()..=i] == needle {
            return Some(i + 1 - needle.len());
        }
        i += 1;
    }
    None
}

fn content_length(head: &[u8]) -> usize {
    let mut pos = 0;
    while pos < head.len() {
        let end =
            head[pos..].iter().position(|&c| c == b'\n').map(|j| pos + j).unwrap_or(head.len());
        let line = &head[pos..end];
        pos = end + 1;
        let Some(colon) = line.iter().position(|&c| c == b':') else { continue };
        let name: Vec<u8> = line[..colon].iter().map(|c| c.to_ascii_lowercase()).collect();
        if name == b"content-length" {
            return std::str::from_utf8(&line[colon + 1..])
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_large_stream_in_chunks() {
        let body = "x".repeat(16_000);
        let one = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\nDate: Tue, 12 Aug 2026 03:20:00 GMT\r\n\r\n{body}",
            body.len()
        );
        let stream = one.repeat(20).into_bytes();
        let mut c = super::Counter::default();
        let mut total = 0;
        for chunk in stream.chunks(64 << 10) {
            total += c.feed(chunk);
        }
        assert_eq!(total, 20);
    }

    #[test]
    fn counts_pipelined_responses() {
        let mut c = super::Counter::default();
        let b = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        assert_eq!(c.feed(b), 2);

        let mut c = super::Counter::default();
        assert_eq!(c.feed(b"HTTP/1.1 200 OK\r\nContent-Len"), 0);
        assert_eq!(c.feed(b"gth: 4\r\n\r\nab"), 0);
        assert_eq!(c.feed(b"cd"), 1);

        let mut c = super::Counter::default();
        assert_eq!(c.feed(b"HTTP/1.1 204 No Content\r\nConnection: keep-alive\r\n\r\n"), 1);
    }
}
