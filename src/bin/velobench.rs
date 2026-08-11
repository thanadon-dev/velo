use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Args {
    host: String,
    port: u16,
    path: String,
    method: String,
    body: String,
    conns: usize,
    secs: u64,
    pipeline: usize,
}

fn parse_args() -> Args {
    let mut a = Args {
        host: "127.0.0.1".into(),
        port: 8080,
        path: "/".into(),
        method: "GET".into(),
        body: String::new(),
        conns: 50,
        secs: 5,
        pipeline: 1,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let mut next = || {
            i += 1;
            argv.get(i).cloned().unwrap_or_default()
        };
        match arg {
            "-c" => a.conns = next().parse().unwrap_or(a.conns),
            "-d" => a.secs = next().parse().unwrap_or(a.secs),
            "-p" => a.pipeline = next().parse().unwrap_or(a.pipeline).max(1),
            "-m" => a.method = next().to_uppercase(),
            "-b" => a.body = next(),
            "-h" | "--help" => {
                eprintln!("usage: velobench [-c conns] [-d secs] [-p depth] [-m method] [-b body] http://host:port/path");
                std::process::exit(0);
            }
            url => {
                let rest = url.strip_prefix("http://").unwrap_or(url);
                let (hostport, path) = match rest.find('/') {
                    Some(j) => (&rest[..j], &rest[j..]),
                    None => (rest, "/"),
                };
                let (h, p) = match hostport.rsplit_once(':') {
                    Some((h, p)) => (h, p.parse().unwrap_or(80)),
                    None => (hostport, 80),
                };
                a.host = h.to_string();
                a.port = p;
                a.path = path.to_string();
            }
        }
        i += 1;
    }
    a
}

fn build_request(a: &Args) -> Vec<u8> {
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: keep-alive\r\n",
        a.method, a.path, a.host, a.port
    );
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

fn main() {
    let a = Arc::new(parse_args());
    let req = Arc::new(build_request(&a));
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));

    let start = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..a.conns {
        let (a, req, stop, done, errors, bytes) =
            (a.clone(), req.clone(), stop.clone(), done.clone(), errors.clone(), bytes.clone());
        handles.push(std::thread::spawn(move || worker(a, req, stop, done, errors, bytes)));
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
    let total = done.load(Ordering::Relaxed);
    println!("{} {} -> {} conns, {:.1}s", a.method, a.path, a.conns, elapsed);
    println!("requests    {total}");
    println!("errors      {}", errors.load(Ordering::Relaxed));
    println!("throughput  {:.0} req/s", total as f64 / elapsed);
    println!("transfer    {:.1} MB/s", bytes.load(Ordering::Relaxed) as f64 / elapsed / 1e6);
    println!(
        "latency     p50 {:.3} ms  p99 {:.3} ms  max {:.3} ms",
        pick(0.50),
        pick(0.99),
        pick(1.0)
    );
}

fn worker(
    a: Arc<Args>,
    req: Arc<Vec<u8>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
) -> Vec<u64> {
    let mut lat = Vec::with_capacity(1 << 16);
    let Ok(mut s) = TcpStream::connect((a.host.as_str(), a.port)) else {
        errors.fetch_add(1, Ordering::Relaxed);
        return lat;
    };
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut batch = Vec::with_capacity(req.len() * a.pipeline);
    for _ in 0..a.pipeline {
        batch.extend_from_slice(&req);
    }
    let mut buf = vec![0u8; 64 << 10];
    let mut local_done = 0u64;
    let mut local_bytes = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        if s.write_all(&batch).is_err() {
            errors.fetch_add(1, Ordering::Relaxed);
            break;
        }
        let mut seen = 0usize;
        while seen < a.pipeline {
            match s.read(&mut buf) {
                Ok(0) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                Ok(n) => {
                    local_bytes += n as u64;
                    seen += count_responses(&buf[..n]);
                }
                Err(_) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
        }
        if seen == 0 {
            break;
        }
        let us = t0.elapsed().as_micros() as u64 / a.pipeline as u64;
        if lat.len() < lat.capacity() {
            lat.push(us);
        }
        local_done += seen as u64;
    }
    done.fetch_add(local_done, Ordering::Relaxed);
    bytes.fetch_add(local_bytes, Ordering::Relaxed);
    lat
}

fn count_responses(b: &[u8]) -> usize {
    let needle = b"HTTP/1.";
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= b.len() {
        if &b[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_pipelined_responses() {
        let b = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        assert_eq!(super::count_responses(b), 2);
        assert_eq!(super::count_responses(b"ok"), 0);
    }
}
