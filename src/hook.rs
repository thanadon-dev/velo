use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub static SENT: AtomicU64 = AtomicU64::new(0);
pub static FAILED: AtomicU64 = AtomicU64::new(0);
pub static DROPPED: AtomicU64 = AtomicU64::new(0);
static PENDING: AtomicUsize = AtomicUsize::new(0);

struct Queue {
    jobs: Mutex<VecDeque<(String, Vec<u8>)>>,
    wake: Condvar,
    depth: usize,
    timeout: Duration,
}

static QUEUE: OnceLock<Queue> = OnceLock::new();

fn env_usize(name: &str, fallback: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(fallback)
}

fn queue() -> &'static Queue {
    QUEUE.get_or_init(|| {
        let q = Queue {
            jobs: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            depth: env_usize("VELO_HOOK_QUEUE", 1024),
            timeout: Duration::from_millis(env_usize("VELO_HOOK_MS", 3000) as u64),
        };
        for _ in 0..env_usize("VELO_HOOK_THREADS", 2) {
            std::thread::spawn(sender);
        }
        q
    })
}

pub fn send(url: &str, body: Vec<u8>) {
    let q = queue();
    let mut jobs = q.jobs.lock().unwrap();
    if jobs.len() >= q.depth {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    jobs.push_back((url.to_string(), body));
    PENDING.fetch_add(1, Ordering::Relaxed);
    drop(jobs);
    q.wake.notify_one();
}

pub fn drain(limit: Duration) {
    if QUEUE.get().is_none() {
        return;
    }
    let deadline = Instant::now() + limit;
    while PENDING.load(Ordering::Relaxed) > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn sender() {
    let q = queue();
    loop {
        let job = {
            let mut jobs = q.jobs.lock().unwrap();
            loop {
                match jobs.pop_front() {
                    Some(job) => break job,
                    None => jobs = q.wake.wait(jobs).unwrap(),
                }
            }
        };
        let ok = deliver(&job.0, &job.1, q.timeout);
        PENDING.fetch_sub(1, Ordering::Relaxed);
        match ok {
            true => SENT.fetch_add(1, Ordering::Relaxed),
            false => FAILED.fetch_add(1, Ordering::Relaxed),
        };
    }
}

pub fn split(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(cut) => (&rest[..cut], &rest[cut..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = match authority.contains(':') {
        true => authority.to_string(),
        false => format!("{authority}:80"),
    };
    Some((host, path.to_string()))
}

fn deliver(url: &str, body: &[u8], timeout: Duration) -> bool {
    let Some((host, path)) = split(url) else {
        return false;
    };
    let Ok(mut addrs) = host.to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = s.set_write_timeout(Some(timeout));
    let _ = s.set_read_timeout(Some(timeout));
    let mut req = Vec::with_capacity(body.len() + host.len() + path.len() + 128);
    req.extend_from_slice(b"POST ");
    req.extend_from_slice(path.as_bytes());
    req.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(b"\r\nContent-Type: application/json\r\nUser-Agent: velo/");
    req.extend_from_slice(crate::VERSION.as_bytes());
    req.extend_from_slice(b"\r\nConnection: close\r\nContent-Length: ");
    req.extend_from_slice(body.len().to_string().as_bytes());
    req.extend_from_slice(b"\r\n\r\n");
    req.extend_from_slice(body);
    if s.write_all(&req).is_err() {
        return false;
    }
    let mut head = [0u8; 16];
    let mut seen = 0;
    while seen < head.len() {
        match s.read(&mut head[seen..]) {
            Ok(0) => break,
            Ok(n) => seen += n,
            Err(_) => return false,
        }
    }
    head[..seen].starts_with(b"HTTP/1.") && matches!(head.get(9), Some(b'2') | Some(b'3'))
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn a_url_splits_into_an_authority_with_a_port_and_a_path() {
        assert_eq!(split("http://a.example/hooks"), Some(("a.example:80".into(), "/hooks".into())));
        assert_eq!(
            split("http://a.example:9000/h?x=1"),
            Some(("a.example:9000".into(), "/h?x=1".into()))
        );
        assert_eq!(split("http://a.example"), Some(("a.example:80".into(), "/".into())));
    }

    #[test]
    fn anything_that_is_not_plain_http_is_refused_rather_than_guessed_at() {
        assert_eq!(split("https://a.example/hooks"), None);
        assert_eq!(split("a.example/hooks"), None);
        assert_eq!(split("http:///hooks"), None);
        assert_eq!(split("http://user:pass@a.example/h"), None);
    }
}
