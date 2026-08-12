use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;
use velo::{compile, Server};

const SRC: &str = r#"
GET  /health      => "ok"
GET  /users       => db.users.all()
GET  /users/:id   => db.users.find(id)
POST /users       => db.users.create(body)
GET  /q           => { a: query.a, n: len(query.a) }
GET  /sorted      => db.users.order(query.by)
"#;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn compiler_survives_mutated_sources() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let base = SRC.as_bytes();
    for _ in 0..2000 {
        let mut src = base.to_vec();
        for _ in 0..1 + rng.below(6) {
            let at = rng.below(src.len());
            match rng.below(3) {
                0 => src[at] = rng.byte(),
                1 => {
                    src.remove(at);
                }
                _ => src.insert(at, rng.byte()),
            }
        }
        let text = String::from_utf8_lossy(&src).into_owned();
        if let Ok(prog) = compile(&text, None) {
            let _ = Server::new(prog);
        }
    }
}

#[test]
fn compiler_survives_random_bytes() {
    let mut rng = Rng(0xfeed_0000_0000_beef);
    for _ in 0..2000 {
        let len = rng.below(64);
        let bytes: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let _ = compile(&text, None);
    }
}

#[test]
fn server_survives_malformed_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let srv = Server::new(compile(SRC, None).unwrap()).unwrap();
    let bg = srv.clone();
    let h = std::thread::spawn(move || bg.serve(listener));

    let pieces: [&[u8]; 12] = [
        b"GET",
        b"GET / HTTP/1.1\r\n",
        b"Host: x\r\n",
        b"Content-Length: 9999999999999999999\r\n",
        b"Content-Length: -1\r\n",
        b"Transfer-Encoding: chunked\r\n",
        b"\r\n",
        b"\n",
        b"POST /users HTTP/1.1\r\nContent-Length: 5\r\n\r\n{",
        b"GET /users/%%%%%% HTTP/1.1\r\nHost: x\r\n\r\n",
        b"GET /q?a=%E0%B9 HTTP/1.1\r\nHost: x\r\n\r\n",
        b"\x00\x01\x02\xff\xfe",
    ];
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for _ in 0..300 {
        let Ok(mut c) = TcpStream::connect(("127.0.0.1", port)) else { continue };
        c.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        c.set_write_timeout(Some(Duration::from_millis(200))).unwrap();
        for _ in 0..1 + rng.below(5) {
            let p = pieces[rng.below(pieces.len())];
            if c.write_all(p).is_err() {
                break;
            }
        }
        let mut buf = [0u8; 1024];
        let _ = c.read(&mut buf);
    }

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 200 OK"), "{out}");
    assert!(out.ends_with("ok"), "{out}");

    srv.shutdown();
    h.join().unwrap().unwrap();
}

#[test]
fn oversized_requests_are_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let srv = Server::new(compile(SRC, None).unwrap()).unwrap();
    let bg = srv.clone();
    let h = std::thread::spawn(move || bg.serve(listener));

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = b"GET /health HTTP/1.1\r\nHost: x\r\n".to_vec();
    for i in 0..400 {
        req.extend_from_slice(format!("X-Pad-{i}: {}\r\n", "p".repeat(64)).as_bytes());
    }
    let _ = c.write_all(&req);
    let mut out = String::new();
    let _ = c.read_to_string(&mut out);
    assert!(out.starts_with("HTTP/1.1 431") || out.is_empty(), "{out}");

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(b"POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 2097152\r\n\r\n").unwrap();
    let mut out = String::new();
    let _ = c.read_to_string(&mut out);
    assert!(out.starts_with("HTTP/1.1 413"), "{out}");

    srv.shutdown();
    h.join().unwrap().unwrap();
}

#[test]
fn slow_clients_are_dropped() {
    std::env::set_var("VELO_HEADER_TIMEOUT", "1");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let srv = Server::new(compile(SRC, None).unwrap()).unwrap();
    assert_eq!(srv.header_secs, 1);
    let bg = srv.clone();
    let h = std::thread::spawn(move || bg.serve(listener));
    std::env::remove_var("VELO_HEADER_TIMEOUT");

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    c.write_all(b"GET /health HTTP/1.1\r\n").unwrap();
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(400));
        if c.write_all(b"X-Drip: 1\r\n").is_err() {
            break;
        }
    }
    let mut out = String::new();
    let _ = c.read_to_string(&mut out);
    assert!(out.is_empty(), "slow client should be dropped, got {out}");

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    assert!(out.ends_with("ok"), "{out}");

    srv.shutdown();
    h.join().unwrap().unwrap();
}
