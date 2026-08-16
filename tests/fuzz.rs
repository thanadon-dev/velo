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

fn iterations(default: usize) -> usize {
    std::env::var("VELO_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

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
        if n == 0 {
            return 0;
        }
        (self.next() % n as u64) as usize
    }
}

#[test]
fn compiler_survives_mutated_sources() {
    let rounds = iterations(2000);
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let base = SRC.as_bytes();
    for _ in 0..rounds {
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
    let rounds = iterations(2000);
    let mut rng = Rng(0xfeed_0000_0000_beef);
    for _ in 0..rounds {
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
    let h = std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));

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
    let h = std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));

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
    let h = std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));
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

#[test]
fn mutated_valid_requests_never_break_the_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let srv = Server::new(compile(SRC, None).unwrap()).unwrap();
    let bg = srv.clone();
    let h = std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));

    let seeds: [&str; 6] = [
        "GET /health HTTP/1.1\r\nHost: x\r\n\r\n",
        "GET /users/12?full=1 HTTP/1.1\r\nHost: x\r\nAccept: */*\r\n\r\n",
        "POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 15\r\n\r\n{\"name\":\"mark\"}",
        "POST /users HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\n{}",
        "HEAD /q?a=1 HTTP/1.1\r\nHost: x\r\nIf-None-Match: \"abc\"\r\n\r\n",
        "GET /sorted?by=name HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n",
    ];

    let mut rng = Rng(0x000a_11ce_5eed_0007);
    for round in 0..iterations(400) {
        let mut req = seeds[rng.below(seeds.len())].as_bytes().to_vec();
        for _ in 0..1 + rng.below(4) {
            if req.is_empty() {
                break;
            }
            let at = rng.below(req.len());
            match rng.below(4) {
                0 => req[at] = rng.byte(),
                1 => {
                    req.remove(at);
                }
                2 => req.insert(at, rng.byte()),
                _ => {
                    let end = (at + 1 + rng.below(8)).min(req.len());
                    req.truncate(end.max(1));
                }
            }
        }
        let Ok(mut c) = TcpStream::connect(("127.0.0.1", port)) else { continue };
        c.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        c.set_write_timeout(Some(Duration::from_millis(200))).unwrap();
        let _ = c.write_all(&req);
        let mut buf = [0u8; 4096];
        if let Ok(n) = c.read(&mut buf) {
            if n > 0 {
                let head = String::from_utf8_lossy(&buf[..n]);
                assert!(
                    head.starts_with("HTTP/1.1 "),
                    "round {round}: answered {head:?} to {:?}",
                    String::from_utf8_lossy(&req)
                );
                let code: u16 = head[9..12].parse().unwrap_or(0);
                assert!((100..=599).contains(&code), "round {round}: status {code}");
            }
        }
    }

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    assert!(out.ends_with("ok"), "{out}");

    srv.shutdown();
    h.join().unwrap().unwrap();
}

const SHAPE_SRC: &str = r#"
POST /add     => db.rows.create(body)
GET  /n       => db.rows.count()
GET  /eq      => db.rows.where("k", query.v).count()
GET  /ne      => db.rows.where("k", "!=", query.v).count()
GET  /both    => db.rows.where("k", query.v).where("g", query.g).count()
GET  /swap    => db.rows.where("g", query.g).where("k", query.v).count()
GET  /sum     => db.rows.where("g", query.g).sum("n")
GET  /list    => db.rows.where("k", query.v).select("id")
GET  /first   => db.rows.where("k", query.v).order("id").first().select("id")
GET  /top     => db.rows.order("k").page(0, query.p).select("id")
GET  /allsort => db.rows.order("k").select("id")
PUT  /up/:id  => db.rows.update(id, body)
PUT  /ups/:id => db.rows.upsert(id, body)
DELETE /del/:id => db.rows.delete(id)
DELETE /delw  => db.rows.delete_where("g", query.g)
"#;

struct Row {
    id: usize,
    json: String,
    k: String,
    g: String,
    n: Option<f64>,
}

fn shape_rows(rng: &mut Rng, count: usize) -> Vec<Row> {
    let ks = ["\"a\"", "\"b\"", "\"c\"", "null", "12", "true", "\"\""];
    let gs = ["\"x\"", "\"y\"", "null"];
    let ns = ["1", "2.5", "-3", "\"7\"", "null", "\"nope\""];
    let mut out = Vec::with_capacity(count);
    for id in 0..count {
        let mut fields: Vec<(&str, String)> = vec![("id", id.to_string())];
        let k = if rng.below(8) == 0 { None } else { Some(ks[rng.below(ks.len())]) };
        let g = if rng.below(8) == 0 { None } else { Some(gs[rng.below(gs.len())]) };
        let n = if rng.below(8) == 0 { None } else { Some(ns[rng.below(ns.len())]) };
        if let Some(v) = k {
            fields.push(("k", v.to_string()));
        }
        if let Some(v) = g {
            fields.push(("g", v.to_string()));
        }
        if let Some(v) = n {
            fields.push(("n", v.to_string()));
        }
        for p in 0..rng.below(4) {
            fields.push(("pad", format!("\"p{p}\"")));
        }
        for at in (1..fields.len()).rev() {
            fields.swap(at, 1 + rng.below(at));
        }
        let body: Vec<String> = fields.iter().map(|(name, v)| format!("\"{name}\":{v}")).collect();
        out.push(Row {
            id,
            json: format!("{{{}}}", body.join(",")),
            k: as_key(k),
            g: as_key(g),
            n: n.filter(|v| !v.starts_with('"')).and_then(|v| v.parse::<f64>().ok()),
        });
    }
    out
}

fn as_key(raw: Option<&str>) -> String {
    match raw {
        None | Some("null") => String::new(),
        Some(v) => v.trim_matches('"').to_string(),
    }
}

fn ask(s: &Server, path: &str) -> String {
    let mut out = Vec::new();
    s.dispatch("GET", path, b"", &mut out);
    String::from_utf8(out).unwrap()
}

fn ids(body: &str) -> Vec<usize> {
    body.split("\"id\":")
        .skip(1)
        .filter_map(|p| p.trim_start_matches('"').split(|c: char| !c.is_ascii_digit()).next())
        .filter(|d| !d.is_empty())
        .map(|d| d.parse().unwrap())
        .collect()
}

fn load(rows: &[Row], order: &[usize]) -> std::sync::Arc<Server> {
    let s = Server::new(compile(SHAPE_SRC, None).unwrap()).unwrap();
    for at in order {
        let mut out = Vec::new();
        let (code, _) = s.dispatch("POST", "/add", rows[*at].json.as_bytes(), &mut out);
        assert_eq!(code, 201, "{}", rows[*at].json);
    }
    s
}

#[test]
fn queries_over_randomly_shaped_rows_agree_with_counting_by_hand() {
    let mut rng = Rng(0x51ed_2701_aa11);
    for round in 0..iterations(3) {
        let rows = shape_rows(&mut rng, 700);
        let forward: Vec<usize> = (0..rows.len()).collect();
        let mut shuffled = forward.clone();
        for at in (1..shuffled.len()).rev() {
            shuffled.swap(at, rng.below(at + 1));
        }
        let s = load(&rows, &forward);
        let other = load(&rows, &shuffled);
        let total = rows.len();
        assert_eq!(ask(&s, "/n"), total.to_string());

        for want in ["a", "b", "c", "12", "true", "", "missing"] {
            let path = format!("/eq?v={want}");
            let by_hand = rows.iter().filter(|r| r.k == want).count();
            let got: usize = ask(&s, &path).parse().unwrap();
            assert_eq!(got, by_hand, "round {round}: eq {want:?}");
            let ne: usize = ask(&s, &format!("/ne?v={want}")).parse().unwrap();
            assert_eq!(got + ne, total, "round {round}: eq + ne must cover every row");
            assert_eq!(
                ask(&other, &path),
                got.to_string(),
                "round {round}: insertion order changed the answer for {want:?}"
            );

            let listed = ids(&ask(&s, &format!("/list?v={want}")));
            let expect: Vec<usize> = rows.iter().filter(|r| r.k == want).map(|r| r.id).collect();
            assert_eq!(listed, expect, "round {round}: list {want:?}");

            let first = ask(&s, &format!("/first?v={want}"));
            match expect.first() {
                Some(id) => assert_eq!(ids(&first), vec![*id], "round {round}: first {want:?}"),
                None => assert!(first.contains("not found"), "round {round}: {first}"),
            }

            for g in ["x", "y", ""] {
                let by_hand = rows.iter().filter(|r| r.k == want && r.g == g).count();
                let both: usize = ask(&s, &format!("/both?v={want}&g={g}")).parse().unwrap();
                let swap: usize = ask(&s, &format!("/swap?v={want}&g={g}")).parse().unwrap();
                assert_eq!((both, swap), (by_hand, by_hand), "round {round}: {want:?} {g:?}");
            }
        }

        for g in ["x", "y", ""] {
            let by_hand: f64 = rows.iter().filter(|r| r.g == g).filter_map(|r| r.n).sum();
            let got: f64 = ask(&s, &format!("/sum?g={g}")).parse().unwrap();
            assert!((got - by_hand).abs() < 1e-9, "round {round}: sum {g:?} {got} vs {by_hand}");
            assert_eq!(ask(&other, &format!("/sum?g={g}")), got.to_string());
        }

        let sorted = ids(&ask(&s, "/allsort"));
        assert_eq!(sorted.len(), total, "round {round}: sorting lost rows");
        let mut seen = sorted.clone();
        seen.sort_unstable();
        assert_eq!(seen, forward, "round {round}: sorting invented or dropped an id");
        for p in [1usize, 7, 100, 700, 900] {
            assert_eq!(
                ids(&ask(&s, &format!("/top?p={p}"))),
                sorted[..p.min(total)].to_vec(),
                "round {round}: top {p} must match the full sort"
            );
        }
    }
}

#[derive(Clone)]
struct Held {
    k: String,
    g: String,
    n: Option<f64>,
}

fn send(s: &Server, method: &str, path: &str, body: &str) -> u16 {
    let mut out = Vec::new();
    s.dispatch(method, path, body.as_bytes(), &mut out).0
}

fn field_patch(rng: &mut Rng, held: &mut Held) -> String {
    let ks = ["\"a\"", "\"b\"", "\"c\"", "null", "12", "true", "\"\""];
    let gs = ["\"x\"", "\"y\"", "null"];
    let ns = ["1", "2.5", "-3", "\"7\"", "null"];
    match rng.below(3) {
        0 => {
            let v = ks[rng.below(ks.len())];
            held.k = as_key(Some(v));
            format!("\"k\":{v}")
        }
        1 => {
            let v = gs[rng.below(gs.len())];
            held.g = as_key(Some(v));
            format!("\"g\":{v}")
        }
        _ => {
            let v = ns[rng.below(ns.len())];
            held.n = (!v.starts_with('"')).then(|| v.parse::<f64>().ok()).flatten();
            format!("\"n\":{v}")
        }
    }
}

fn check(s: &Server, held: &std::collections::BTreeMap<usize, Held>, note: &str) {
    assert!(held.len() >= 512, "{note}: {} rows is under the index threshold", held.len());
    assert_eq!(ask(s, "/n"), held.len().to_string(), "{note}: count");
    for want in ["a", "b", "c", "12", "true", "", "gone"] {
        let expect: Vec<usize> =
            held.iter().filter(|(_, h)| h.k == want).map(|(id, _)| *id).collect();
        assert_eq!(
            ask(s, &format!("/eq?v={want}")),
            expect.len().to_string(),
            "{note}: count of {want:?}"
        );
        assert_eq!(ids(&ask(s, &format!("/list?v={want}"))), expect, "{note}: rows of {want:?}");
        for g in ["x", "y", ""] {
            let n = held.values().filter(|h| h.k == want && h.g == g).count();
            assert_eq!(
                ask(s, &format!("/both?v={want}&g={g}")),
                n.to_string(),
                "{note}: {want:?} and {g:?}"
            );
        }
    }
    for g in ["x", "y", ""] {
        let by_hand: f64 = held.values().filter(|h| h.g == g).filter_map(|h| h.n).sum();
        let got: f64 = ask(s, &format!("/sum?g={g}")).parse().unwrap();
        assert!((got - by_hand).abs() < 1e-9, "{note}: sum {g:?} {got} vs {by_hand}");
    }
    let mut sorted = ids(&ask(s, "/allsort"));
    assert_eq!(sorted.len(), held.len(), "{note}: sorting lost rows");
    sorted.sort_unstable();
    assert_eq!(sorted, held.keys().copied().collect::<Vec<_>>(), "{note}: wrong set of rows");
}

#[test]
fn writes_keep_every_index_and_cache_honest() {
    let mut rng = Rng(0x9e37_79b9_7f4a);
    for round in 0..iterations(2) {
        let rows = shape_rows(&mut rng, 1200);
        let order: Vec<usize> = (0..rows.len()).collect();
        let s = load(&rows, &order);
        let mut held: std::collections::BTreeMap<usize, Held> =
            rows.iter().map(|r| (r.id, Held { k: r.k.clone(), g: r.g.clone(), n: r.n })).collect();
        let mut next = rows.len();
        check(&s, &held, &format!("round {round} start"));

        for step in 0..600 {
            let ids_now: Vec<usize> = held.keys().copied().collect();
            let roll = if held.len() < 800 {
                5 + rng.below(4)
            } else if held.len() > 1100 {
                rng.below(10)
            } else {
                rng.below(9)
            };
            match roll {
                0..=2 if !ids_now.is_empty() => {
                    let id = ids_now[rng.below(ids_now.len())];
                    let mut h = held[&id].clone();
                    let patch = field_patch(&mut rng, &mut h);
                    assert_eq!(send(&s, "PUT", &format!("/up/{id}"), &format!("{{{patch}}}")), 200);
                    held.insert(id, h);
                }
                3..=4 if !ids_now.is_empty() => {
                    let id = ids_now[rng.below(ids_now.len())];
                    assert_eq!(send(&s, "DELETE", &format!("/del/{id}"), ""), 200);
                    held.remove(&id);
                    assert_eq!(send(&s, "DELETE", &format!("/del/{id}"), ""), 404, "already gone");
                }
                5..=6 => {
                    let mut h = Held { k: String::new(), g: String::new(), n: None };
                    let patch = field_patch(&mut rng, &mut h);
                    let body = format!("{{\"id\":{next},{patch}}}");
                    assert_eq!(send(&s, "POST", "/add", &body), 201);
                    held.insert(next, h);
                    next += 1;
                }
                7..=8 => {
                    let reuse = !ids_now.is_empty() && rng.below(2) == 0;
                    let id = if reuse { ids_now[rng.below(ids_now.len())] } else { next };
                    let mut h = held.get(&id).cloned().unwrap_or(Held {
                        k: String::new(),
                        g: String::new(),
                        n: None,
                    });
                    let patch = field_patch(&mut rng, &mut h);
                    let code = send(&s, "PUT", &format!("/ups/{id}"), &format!("{{{patch}}}"));
                    assert_eq!(code, 200);
                    held.insert(id, h);
                    if !reuse {
                        next += 1;
                    }
                }
                9 => {
                    let g = ["x", "y", ""][rng.below(3)];
                    let gone: Vec<usize> =
                        held.iter().filter(|(_, h)| h.g == g).map(|(id, _)| *id).collect();
                    assert_eq!(send(&s, "DELETE", &format!("/delw?g={g}"), ""), 200);
                    for id in gone {
                        held.remove(&id);
                    }
                }
                _ => {
                    let mut h = Held { k: String::new(), g: String::new(), n: None };
                    let patch = field_patch(&mut rng, &mut h);
                    let body = format!("{{\"id\":{next},{patch}}}");
                    assert_eq!(send(&s, "POST", "/add", &body), 201);
                    held.insert(next, h);
                    next += 1;
                }
            }
            let touched = ["a", "b", "c", "12", "true", ""][step % 6];
            let expect = held.values().filter(|h| h.k == touched).count();
            assert_eq!(
                ask(&s, &format!("/eq?v={touched}")),
                expect.to_string(),
                "round {round} step {step}: a read straight after a write must see it"
            );
            assert_eq!(ask(&s, "/n"), held.len().to_string(), "round {round} step {step}: count");
            if step % 30 == 29 {
                check(&s, &held, &format!("round {round} step {step}"));
            }
        }
        check(&s, &held, &format!("round {round} end"));
    }
}
