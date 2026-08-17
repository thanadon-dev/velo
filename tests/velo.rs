use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;
use velo::http::{Ctype, JSON, TEXT};
use velo::value::parse_json;
use velo::{compile, Server, Value};

const SRC: &str = r#"
GET  /health          => "ok"
GET  /version         => { name: "velo", n: 2 }
GET  /users           => db.users.all()
GET  /users/:id       => db.users.find(id)
POST /users           => db.users.create(body)
PUT  /users/:id       => db.users.update(id, body)
PUT  /put/:id         => db.users.upsert(id, body)
POST /uniq            => db.users.create(body, "email")
POST /uniq2           => db.users.create(body, "email", "phone")
POST /bump/:id        => db.users.incr(id, "score") : 200
POST /bumpby/:id      => db.users.incr(id, "score", query.by) : 200
POST /bumpbad/:id     => db.users.incr(id, "name") : 200
POST /bumpid/:id      => db.users.incr(id, "id") : 200
DELETE /wipe          => db.users.clear()
DELETE /team/:t       => db.users.delete_where("team", t)
PUT  /keyed/:id       => db.keyed.upsert(id, body)
DELETE /users/:id     => db.users.delete(id)
GET  /stats           => { users: db.users.count() }
GET  /echo/:a/x/:b    => { a: a, b: b }
POST /name            => body.name
GET  /list            => [1, 2, "three", true, null]
GET  /search          => db.users.where("name", query.name)
GET  /find            => db.users.search("name", query.q)
GET  /math            => { sum: 2 + 3 * 4, grouped: (2 + 3) * 4, div: 10 / 4, neg: 0 - 7, join: "a" + "b" }
GET  /calc/:n         => { n: n, doubled: n * 2, next: n + 1, big: n > 10 }
GET  /limited         => db.users.page(0, query.limit) when query.limit < 100 else 400
GET  /totals          => { n: db.users.count(), sum: db.users.sum("score"), avg: db.users.avg("score"), lo: db.users.min("score"), hi: db.users.max("score") }
GET  /q               => { limit: query.limit, tag: query.tag }
GET  /raw/:v          => { v: v }
DELETE /gone/:id      => { id: id } : 204
POST /accepted        => "queued" : 202
GET  /paged           => db.users.page(query.offset, query.limit)
GET  /byname/:n       => db.users.first("name", n)
POST /keyed           => db.keyed.create(body)
GET  /keyed/:id       => db.keyed.find(id)
GET  /time            => { at: now(), iso: date(now()), fixed: date(1755000000000), bad: date("x") }
GET  /id              => uuid()
GET  /sizes           => { s: len("abc"), a: len([1,2]), n: len(null) }
GET  /home            => env("VELO_TEST_ENV")
GET  /sorted          => db.users.order("name")
GET  /rsorted         => db.users.order("-id")
GET  /whoami          => { agent: header.user_agent, auth: header.authorization }
GET  /tenant          => db.users.where("team", header.x_team)
GET  /admin           => db.users.all() when header.authorization == "Bearer secret"
GET  /gated           => "in" when header.x_key
GET  /both            => "both" when header.x_key and header.x_team else 400
GET  /either          => "either" when header.x_key or header.x_team
POST /validated       => db.users.create(body) when body.name else 400
GET  /forbidden       => "secret" when header.x_key == "root" else 403
DELETE /purge/:id     => db.users.delete(id) : 204 when header.x_key != "block"
POST /events          => db.events.create({ at: now(), id: uuid(), data: body })
"#;

fn server() -> Arc<Server> {
    Server::new(compile(SRC, None).unwrap()).unwrap()
}

fn call(s: &Server, method: &str, path: &str, body: &str) -> (u16, String, Ctype) {
    let mut out = Vec::new();
    let (status, ct) = s.dispatch(method, path, body.as_bytes(), &mut out);
    (status, String::from_utf8(out).unwrap(), ct)
}

#[test]
fn const_routes_fold() {
    let prog = compile(SRC, None).unwrap();
    let health = prog.routes.iter().find(|r| r.pattern == "/health").unwrap();
    assert_eq!(health.konst.as_deref(), Some(b"ok".as_slice()));
    assert_eq!(health.const_ctype, TEXT);
    let version = prog.routes.iter().find(|r| r.pattern == "/version").unwrap();
    assert_eq!(version.konst.as_deref(), Some(br#"{"name":"velo","n":2}"#.as_slice()));
    let users =
        prog.routes.iter().find(|r| r.pattern == "/users" && r.method.name() == "GET").unwrap();
    assert!(users.konst.is_none());
}

#[test]
fn crud_roundtrip() {
    let s = server();
    assert_eq!(call(&s, "GET", "/users", ""), (200, "[]".into(), JSON));

    let (status, body, _) = call(&s, "POST", "/users", r#"{"name":"mark"}"#);
    assert_eq!(status, 201);
    assert_eq!(body, r#"{"id":1,"name":"mark"}"#);

    assert_eq!(call(&s, "GET", "/users/1", "").1, r#"{"id":1,"name":"mark"}"#);
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":1}"#);

    let (status, body, _) = call(&s, "PUT", "/users/1", r#"{"name":"m","age":3}"#);
    assert_eq!(status, 200);
    assert_eq!(body, r#"{"id":1,"name":"m","age":3}"#);

    assert_eq!(call(&s, "DELETE", "/users/1", "").1, r#"{"deleted":true}"#);
    assert_eq!(call(&s, "GET", "/users/1", "").0, 404);
    assert_eq!(call(&s, "GET", "/users", "").1, "[]");
}

#[test]
fn params_and_body_fields() {
    let s = server();
    assert_eq!(call(&s, "GET", "/echo/1/x/2", "").1, r#"{"a":"1","b":"2"}"#);
    assert_eq!(call(&s, "POST", "/name", r#"{"name":"velo"}"#), (201, "velo".into(), TEXT));
}

#[test]
fn errors() {
    let s = server();
    assert_eq!(call(&s, "GET", "/nope", "").0, 404);
    assert_eq!(call(&s, "PATCH", "/health", "").0, 405);
    assert_eq!(call(&s, "POST", "/users", "{oops").0, 400);
    assert_eq!(call(&s, "POST", "/users", "").0, 400);
    assert_eq!(call(&s, "GET", "/users/999", "").0, 404);
}

#[test]
fn query_string_and_trailing_slash() {
    let s = server();
    assert_eq!(call(&s, "GET", "/health?x=1", "").1, "ok");
    assert_eq!(call(&s, "GET", "/health/", "").1, "ok");
    assert_eq!(call(&s, "GET", "/users/7?full=1", "").0, 404);
}

#[test]
fn compile_errors() {
    assert!(compile("GET /a => nope()", None).is_err());
    assert!(compile("FLY /a => 1", None).is_err());
    assert!(compile("GET /a => db.x.find()", None).is_err());
    assert!(compile("GET /a => 1\nGET /a => 2", None).is_ok());
    assert!(Server::new(compile("GET /a => 1\nGET /a => 2", None).unwrap()).is_err());
    assert!(compile("", None).is_err());
    assert!(compile("GET /a/: => 1", None).is_err());
}

#[test]
fn json_roundtrip() {
    let raw = r#"{"a":[1,2.5,"x\nA😀"],"b":{"c":true},"d":null}"#;
    let v = parse_json(raw.as_bytes()).unwrap();
    let out = v.to_json();
    let again = parse_json(&out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), String::from_utf8(again.to_json()).unwrap());
    assert!(matches!(v.get("b").get("c"), Value::Bool(true)));
    assert!(parse_json(b"{").is_err());
    assert!(parse_json(b"1 2").is_err());
}

#[test]
fn json_escapes() {
    let v = Value::str("a\"b\\c\nd\te\u{1}");
    assert_eq!(String::from_utf8(v.to_json()).unwrap(), "\"a\\\"b\\\\c\\nd\\te\\u0001\"");
}

fn spawn() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = server();
    std::thread::spawn(move || {
        let _ = s.serve(velo::socket::Listener::Tcp(listener));
    });
    port
}

fn raw(port: u16, req: &[u8]) -> String {
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.write_all(req).unwrap();
    c.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut out = String::new();
    let mut buf = [0u8; 4096];
    loop {
        match c.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.contains("\r\n\r\n") && !out.ends_with("\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    out
}

#[test]
fn http_get() {
    let port = spawn();
    let res = raw(port, b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 200 OK\r\n"), "{res}");
    assert!(res.contains("Content-Type: text/plain"), "{res}");
    assert!(res.contains("Content-Length: 2"), "{res}");
    assert!(res.ends_with("ok"), "{res}");
}

#[test]
fn http_post_body() {
    let port = spawn();
    let res = raw(
        port,
        b"POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"name\":\"mark\"}",
    );
    assert!(res.starts_with("HTTP/1.1 201 Created"), "{res}");
    assert!(res.ends_with(r#"{"id":1,"name":"mark"}"#), "{res}");
}

#[test]
fn http_keepalive_pipelined() {
    let port = spawn();
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    c.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\nGET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut out = String::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = c.read(&mut buf) {
        if n == 0 {
            break;
        }
        out.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert_eq!(out.matches("HTTP/1.1 200 OK").count(), 2, "{out}");
    assert!(out.contains("Connection: close"), "{out}");
}

#[test]
fn http_head_and_chunked() {
    let port = spawn();
    let res = raw(port, b"HEAD /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.contains("Content-Length: 2"), "{res}");
    assert!(!res.ends_with("ok"), "{res}");

    let res = raw(
        port,
        b"POST /users HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
    );
    assert!(res.starts_with("HTTP/1.1 411"), "{res}");
}

#[test]
fn http_split_request() {
    let port = spawn();
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    c.write_all(b"POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"name\"")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    c.write_all(b":\"mark\"}").unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 201 Created"), "{out}");
}

#[test]
fn create_keeps_a_named_field_unique() {
    let s = server();
    assert_eq!(call(&s, "POST", "/uniq", r#"{"email":"a@x","name":"a"}"#).0, 201);
    assert_eq!(call(&s, "POST", "/uniq", r#"{"email":"a@x","name":"b"}"#).0, 409);
    assert_eq!(call(&s, "POST", "/uniq", r#"{"email":"b@x","name":"b"}"#).0, 201);
    assert_eq!(call(&s, "POST", "/uniq", r#"{"name":"no email"}"#).0, 201);
    assert_eq!(call(&s, "POST", "/uniq", r#"{"name":"also none"}"#).0, 201);
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":4}"#);
    assert_eq!(call(&s, "POST", "/uniq2", r#"{"email":"c@x","phone":"1"}"#).0, 201);
    assert_eq!(call(&s, "POST", "/uniq2", r#"{"email":"d@x","phone":"1"}"#).0, 409);
    assert_eq!(call(&s, "POST", "/uniq2", r#"{"email":"c@x","phone":"2"}"#).0, 409);
    assert_eq!(call(&s, "POST", "/uniq2", r#"{"email":"d@x","phone":"2"}"#).0, 201);
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":6}"#);
    assert_eq!(call(&s, "POST", "/users", r#"{"email":"a@x"}"#).0, 201);
}

#[test]
fn unique_holds_once_the_index_is_carrying_the_field() {
    let s = server();
    for i in 0..700 {
        let body = format!(r#"{{"email":"u{i}@x"}}"#);
        assert_eq!(call(&s, "POST", "/uniq", &body).0, 201, "row {i}");
    }
    assert_eq!(call(&s, "POST", "/uniq", r#"{"email":"u0@x"}"#).0, 409);
    assert_eq!(call(&s, "POST", "/uniq", r#"{"email":"u699@x"}"#).0, 409);
    assert_eq!(call(&s, "POST", "/uniq", r#"{"email":"u700@x"}"#).0, 201);
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":701}"#);
}

#[test]
fn concurrent_signups_with_one_email_leave_one_row() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 200;
    let s = server();
    let gate = Arc::new(std::sync::Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let s = s.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                let mut won = 0;
                for round in 0..ROUNDS {
                    let body = format!(r#"{{"email":"e{round}@x"}}"#);
                    gate.wait();
                    let mut out = Vec::new();
                    let (status, _) = s.dispatch("POST", "/uniq", body.as_bytes(), &mut out);
                    if status == 201 {
                        won += 1;
                    }
                }
                won
            })
        })
        .collect();
    let won: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(won, ROUNDS, "every round must be won exactly once");
    assert_eq!(call(&s, "GET", "/stats", "").1, format!(r#"{{"users":{ROUNDS}}}"#));
}

#[test]
fn incr_counts_and_refuses_what_it_cannot_add_to() {
    let s = server();
    call(&s, "POST", "/users", r#"{"id":"a","name":"mark"}"#);
    assert_eq!(call(&s, "POST", "/bump/a", "").1, r#"{"id":"a","name":"mark","score":1}"#);
    assert_eq!(call(&s, "POST", "/bump/a", "").1, r#"{"id":"a","name":"mark","score":2}"#);
    assert_eq!(call(&s, "POST", "/bumpby/a?by=10", "").1, r#"{"id":"a","name":"mark","score":12}"#);
    assert_eq!(call(&s, "POST", "/bumpby/a?by=-5", "").1, r#"{"id":"a","name":"mark","score":7}"#);
    assert_eq!(call(&s, "GET", "/users/a", "").1, r#"{"id":"a","name":"mark","score":7}"#);
    assert_eq!(call(&s, "POST", "/bump/nobody", "").0, 404);
    assert_eq!(call(&s, "POST", "/bumpbad/a", "").0, 409);
    assert!(call(&s, "POST", "/bumpbad/a", "").1.contains("not a number"));
    assert_eq!(call(&s, "POST", "/bumpid/a", "").0, 409);
    assert_eq!(call(&s, "POST", "/bumpby/a?by=x", "").0, 400);
    assert_eq!(call(&s, "GET", "/users/a", "").1, r#"{"id":"a","name":"mark","score":7}"#);
    assert!(compile(r#"GET /a => db.x.incr("k")"#, None).is_err());
    assert!(compile(r#"GET /a => db.x.incr("k", "f", 1, 2)"#, None).is_err());
}

#[test]
fn incr_never_loses_a_count_under_concurrency() {
    let s = server();
    call(&s, "POST", "/users", r#"{"id":"a","score":0}"#);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let s = s.clone();
            std::thread::spawn(move || {
                for _ in 0..250 {
                    let mut out = Vec::new();
                    s.dispatch("POST", "/bump/a", b"", &mut out);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(call(&s, "GET", "/users/a", "").1, r#"{"id":"a","score":2000}"#);
}

#[test]
fn concurrent_creates_are_unique() {
    let s = server();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let s = s.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    let mut out = Vec::new();
                    s.dispatch("POST", "/users", br#"{"n":1}"#, &mut out);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":400}"#);
    let (_, body, _) = call(&s, "GET", "/users/400", "");
    assert!(body.contains(r#""id":400"#), "{body}");
}

#[test]
fn http_many_connections() {
    let port = spawn();
    let handles: Vec<_> = (0..100)
        .map(|_| {
            std::thread::spawn(move || {
                let res =
                    raw(port, b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
                assert!(res.ends_with("ok"), "{res}");
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn query_params() {
    let s = server();
    assert_eq!(call(&s, "GET", "/q?limit=10&tag=a+b", "").1, r#"{"limit":"10","tag":"a b"}"#);
    assert_eq!(call(&s, "GET", "/q", "").1, r#"{"limit":null,"tag":null}"#);
    assert_eq!(
        call(&s, "GET", "/q?tag=%E0%B9%84%E0%B8%97%E0%B8%A2", "").1,
        "{\"limit\":null,\"tag\":\"ไทย\"}"
    );
}

#[test]
fn path_params_percent_decoded() {
    let s = server();
    assert_eq!(call(&s, "GET", "/raw/a%20b", "").1, r#"{"v":"a b"}"#);
    assert_eq!(call(&s, "GET", "/raw/plain", "").1, r#"{"v":"plain"}"#);
}

#[test]
fn where_filter() {
    let s = server();
    call(&s, "POST", "/users", r#"{"name":"a","team":"x"}"#);
    call(&s, "POST", "/users", r#"{"name":"b","team":"y"}"#);
    call(&s, "POST", "/users", r#"{"name":"a","team":"z"}"#);
    let (status, body, _) = call(&s, "GET", "/search?name=a", "");
    assert_eq!(status, 200);
    assert_eq!(body, r#"[{"id":1,"name":"a","team":"x"},{"id":3,"name":"a","team":"z"}]"#);
    assert_eq!(call(&s, "GET", "/search?name=zz", "").1, "[]");
}

#[test]
fn store_persistence_roundtrip() {
    let store = velo::Store::new();
    let prog = compile(SRC, Some(store.clone())).unwrap();
    let s = Server::new(prog).unwrap();
    call(&s, "POST", "/users", r#"{"name":"a"}"#);
    call(&s, "POST", "/users", r#"{"name":"b"}"#);
    call(&s, "DELETE", "/users/1", "");
    assert!(store.take_dirty());
    assert!(!store.take_dirty());

    let path = std::env::temp_dir().join(format!("velo-test-{}.json", std::process::id()));
    store.save_to(&path).unwrap();

    let store2 = velo::Store::new();
    let prog2 = compile(SRC, Some(store2.clone())).unwrap();
    let s2 = Server::new(prog2).unwrap();
    store2.load_file(&path).unwrap();
    assert_eq!(call(&s2, "GET", "/users", "").1, r#"[{"id":2,"name":"b"}]"#);
    assert_eq!(call(&s2, "GET", "/users/2", "").1, r#"{"id":2,"name":"b"}"#);
    assert_eq!(call(&s2, "GET", "/users/1", "").0, 404);
    let (_, body, _) = call(&s2, "POST", "/users", r#"{"name":"c"}"#);
    assert_eq!(body, r#"{"id":3,"name":"c"}"#);

    let missing = std::env::temp_dir().join("velo-test-does-not-exist.json");
    assert!(velo::Store::new().load_file(&missing).is_ok());
    assert!(velo::Store::new().load_json(b"nope").is_err());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn status_override_and_paging() {
    let s = server();
    assert_eq!(call(&s, "DELETE", "/gone/7", "").0, 204);
    assert_eq!(call(&s, "POST", "/accepted", "").0, 202);
    for i in 0..5 {
        call(&s, "POST", "/users", &format!(r#"{{"n":{i}}}"#));
    }
    assert_eq!(
        call(&s, "GET", "/paged?offset=1&limit=2", "").1,
        r#"[{"id":2,"n":1},{"id":3,"n":2}]"#
    );
    assert_eq!(call(&s, "GET", "/paged?offset=4&limit=10", "").1, r#"[{"id":5,"n":4}]"#);
    assert_eq!(call(&s, "GET", "/paged?offset=99&limit=2", "").1, "[]");
    assert_eq!(call(&s, "GET", "/paged", "").1.matches(r#""id""#).count(), 5);
    assert!(compile("GET /a => 1 : 999", None).is_err());
}

#[test]
fn http_204_has_no_body() {
    let port = spawn();
    let res = raw(port, b"DELETE /gone/1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 204 No Content"), "{res}");
    assert!(!res.contains("Content-Length"), "{res}");
    assert!(res.ends_with("\r\n\r\n"), "{res}");
}

#[test]
fn list_cache_invalidates_on_write() {
    let s = server();
    assert_eq!(call(&s, "GET", "/users", "").1, "[]");
    call(&s, "POST", "/users", r#"{"name":"a"}"#);
    assert_eq!(call(&s, "GET", "/users", "").1, r#"[{"id":1,"name":"a"}]"#);
    assert_eq!(call(&s, "GET", "/users", "").1, r#"[{"id":1,"name":"a"}]"#);
    call(&s, "PUT", "/users/1", r#"{"name":"b"}"#);
    assert_eq!(call(&s, "GET", "/users", "").1, r#"[{"id":1,"name":"b"}]"#);
    call(&s, "POST", "/users", r#"{"name":"c"}"#);
    assert_eq!(call(&s, "GET", "/users", "").1, r#"[{"id":1,"name":"b"},{"id":2,"name":"c"}]"#);
    call(&s, "DELETE", "/users/1", "");
    assert_eq!(call(&s, "GET", "/users", "").1, r#"[{"id":2,"name":"c"}]"#);
}

#[test]
fn serve_stops_on_shutdown() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = server();
    let s2 = s.clone();
    let h = std::thread::spawn(move || s2.serve(velo::socket::Listener::Tcp(listener)));
    let res = raw(port, b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.ends_with("ok"), "{res}");
    s.shutdown();
    h.join().unwrap().unwrap();
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err() || s.stopping());
}

#[test]
fn builtins() {
    std::env::set_var("VELO_TEST_ENV", "set");
    let s = Server::new(compile(SRC, None).unwrap()).unwrap();
    assert_eq!(call(&s, "GET", "/sizes", "").1, r#"{"s":3,"a":2,"n":0}"#);
    assert_eq!(call(&s, "GET", "/home", "").1, "set");

    let (_, id, _) = call(&s, "GET", "/id", "");
    assert_eq!(id.len(), 36, "{id}");
    assert_eq!(id.as_bytes()[14], b'4');
    let (_, id2, _) = call(&s, "GET", "/id", "");
    assert_ne!(id, id2);

    let (_, body, _) = call(&s, "GET", "/time", "");
    let at: u64 = body
        .trim_start_matches("{\"at\":")
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap_or_else(|e| panic!("{body}: {e}"));
    assert!(at > 1_700_000_000_000, "{at}");
    assert!(body.contains(r#""fixed":"2025-08-12T12:00:00Z""#), "{body}");
    assert!(body.contains(r#""bad":null"#), "{body}");
    let iso = body.split(r#""iso":""#).nth(1).unwrap();
    assert!(iso.starts_with("20") && iso.contains('T'), "{body}");

    let (status, ev, _) = call(&s, "POST", "/events", r#"{"kind":"ping"}"#);
    assert_eq!(status, 201);
    assert!(ev.contains(r#""data":{"kind":"ping"}"#), "{ev}");

    assert!(compile("GET /a => nope()", None).is_err());
    assert!(compile("GET /a => len()", None).is_err());
    assert!(compile("GET /a => now(1)", None).is_err());
}

#[test]
fn cors_preflight_and_headers() {
    let mut s = Server::new(compile(SRC, None).unwrap()).unwrap();
    {
        let srv = Arc::get_mut(&mut s).unwrap();
        srv.cors = true;
        srv.extra_headers = b"Access-Control-Allow-Origin: *\r\n".to_vec();
    }
    assert_eq!(call(&s, "OPTIONS", "/users", ""), (204, String::new(), JSON));
    assert_eq!(call(&s, "GET", "/nope", "").0, 404);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let s2 = s.clone();
    std::thread::spawn(move || s2.serve(velo::socket::Listener::Tcp(listener)));
    let res = raw(port, b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.contains("Access-Control-Allow-Origin: *"), "{res}");
    assert!(res.ends_with("ok"), "{res}");
    let pre = raw(port, b"OPTIONS /users HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(pre.starts_with("HTTP/1.1 204"), "{pre}");
    assert!(pre.contains("Access-Control-Allow-Origin: *"), "{pre}");
    assert!(!pre.contains("Content-Length"), "{pre}");
    s.shutdown();
}

#[test]
fn order_sorts_rows() {
    let s = server();
    call(&s, "POST", "/users", r#"{"name":"c"}"#);
    call(&s, "POST", "/users", r#"{"name":"a"}"#);
    call(&s, "POST", "/users", r#"{"name":"b"}"#);
    assert_eq!(
        call(&s, "GET", "/sorted", "").1,
        r#"[{"id":2,"name":"a"},{"id":3,"name":"b"},{"id":1,"name":"c"}]"#
    );
    assert_eq!(
        call(&s, "GET", "/rsorted", "").1,
        r#"[{"id":3,"name":"b"},{"id":2,"name":"a"},{"id":1,"name":"c"}]"#
    );
}

#[test]
fn compile_error_shows_source_line() {
    let err = match compile("GET /a => \"ok\"\nGET /users => user.all()", None) {
        Err(e) => e,
        Ok(_) => panic!("expected a compile error"),
    };
    assert!(err.starts_with("line 2:15: unknown identifier"), "{err}");
    assert!(err.contains("2 | GET /users => user.all()"), "{err}");
    assert!(err.trim_end().ends_with('^'), "{err}");
}

#[test]
fn http_sends_date_and_rejects_duplicate_length() {
    let port = spawn();
    let res = raw(port, b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.contains("Date: "), "{res}");
    assert!(res.contains(" GMT\r\n"), "{res}");

    let res = raw(
        port,
        b"POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\nContent-Length: 9\r\n\r\n{}",
    );
    assert!(res.starts_with("HTTP/1.1 400"), "{res}");

    let res = raw(
        port,
        b"POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"name\":\"a\"}",
    );
    assert!(res.starts_with("HTTP/1.1 201"), "{res}");
}

#[test]
fn sorted_cache_invalidates() {
    let s = server();
    call(&s, "POST", "/users", r#"{"name":"c"}"#);
    call(&s, "POST", "/users", r#"{"name":"a"}"#);
    assert_eq!(call(&s, "GET", "/sorted", "").1, r#"[{"id":2,"name":"a"},{"id":1,"name":"c"}]"#);
    assert_eq!(call(&s, "GET", "/sorted", "").1, r#"[{"id":2,"name":"a"},{"id":1,"name":"c"}]"#);
    call(&s, "POST", "/users", r#"{"name":"b"}"#);
    assert_eq!(
        call(&s, "GET", "/sorted", "").1,
        r#"[{"id":2,"name":"a"},{"id":3,"name":"b"},{"id":1,"name":"c"}]"#
    );
    call(&s, "DELETE", "/users/3", "");
    assert_eq!(call(&s, "GET", "/sorted", "").1, r#"[{"id":2,"name":"a"},{"id":1,"name":"c"}]"#);
}

#[test]
fn request_headers() {
    let s = server();
    let mut out = Vec::new();
    let raw =
        b"GET /whoami HTTP/1.1\r\nHost: x\r\nUser-Agent: velo/1\r\nAuthorization: Bearer t\r\n\r\n";
    let (status, _) = s.handle("GET", "/whoami", b"", raw, &mut out);
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8(out).unwrap(), r#"{"agent":"velo/1","auth":"Bearer t"}"#);

    let mut out = Vec::new();
    s.handle("GET", "/whoami", b"", b"GET /whoami HTTP/1.1\r\n\r\n", &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), r#"{"agent":null,"auth":null}"#);
}

#[test]
fn http_header_routing_end_to_end() {
    let s = server();
    call(&s, "POST", "/users", r#"{"name":"a","team":"red"}"#);
    call(&s, "POST", "/users", r#"{"name":"b","team":"blue"}"#);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bg = s.clone();
    std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));
    let res =
        raw(port, b"GET /tenant HTTP/1.1\r\nHost: x\r\nX-Team: blue\r\nConnection: close\r\n\r\n");
    assert!(res.ends_with(r#"[{"id":2,"name":"b","team":"blue"}]"#), "{res}");
    s.shutdown();
}

#[test]
fn guards_gate_routes() {
    let s = server();
    call(&s, "POST", "/users", r#"{"name":"a"}"#);

    let hdr = |h: &str| format!("GET /admin HTTP/1.1\r\nHost: x\r\n{h}\r\n\r\n");
    let mut out = Vec::new();
    let raw = hdr("Authorization: Bearer secret");
    let (status, _) = s.handle("GET", "/admin", b"", raw.as_bytes(), &mut out);
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8(out).unwrap(), r#"[{"id":1,"name":"a"}]"#);

    let mut out = Vec::new();
    let raw = hdr("Authorization: Bearer wrong");
    let (status, _) = s.handle("GET", "/admin", b"", raw.as_bytes(), &mut out);
    assert_eq!(status, 401);
    assert_eq!(String::from_utf8(out).unwrap(), r#"{"error":"unauthorized"}"#);

    let mut out = Vec::new();
    let (status, _) = s.handle("GET", "/admin", b"", b"GET /admin HTTP/1.1\r\n\r\n", &mut out);
    assert_eq!(status, 401);

    let mut out = Vec::new();
    let raw = "GET /gated HTTP/1.1\r\nX-Key: anything\r\n\r\n";
    let (status, _) = s.handle("GET", "/gated", b"", raw.as_bytes(), &mut out);
    assert_eq!((status, String::from_utf8(out).unwrap()), (200, "in".to_string()));

    let mut out = Vec::new();
    let raw = "GET /gated HTTP/1.1\r\nX-Key: \r\n\r\n";
    let (status, _) = s.handle("GET", "/gated", b"", raw.as_bytes(), &mut out);
    assert_eq!(status, 401);

    let mut out = Vec::new();
    let raw = "DELETE /purge/1 HTTP/1.1\r\nX-Key: block\r\n\r\n";
    let (status, _) = s.handle("DELETE", "/purge/1", b"", raw.as_bytes(), &mut out);
    assert_eq!(status, 401);

    let mut out = Vec::new();
    let raw = "DELETE /purge/1 HTTP/1.1\r\nX-Key: ok\r\n\r\n";
    let (status, _) = s.handle("DELETE", "/purge/1", b"", raw.as_bytes(), &mut out);
    assert_eq!(status, 204);
    assert_eq!(call(&s, "GET", "/users", "").1, "[]");
}

#[test]
fn guarded_routes_are_not_const_folded() {
    let prog = compile(SRC, None).unwrap();
    let gated = prog.routes.iter().find(|r| r.pattern == "/gated").unwrap();
    assert!(gated.konst.is_none());
    assert!(gated.guard.is_some());
    let health = prog.routes.iter().find(|r| r.pattern == "/health").unwrap();
    assert!(health.guard.is_none());
    assert!(health.konst.is_some());
}

#[test]
fn first_and_client_ids() {
    let s = server();
    call(&s, "POST", "/users", r#"{"name":"a"}"#);
    call(&s, "POST", "/users", r#"{"name":"b"}"#);
    assert_eq!(call(&s, "GET", "/byname/b", "").1, r#"{"id":2,"name":"b"}"#);
    assert_eq!(call(&s, "GET", "/byname/zz", "").0, 404);

    let (status, body, _) = call(&s, "POST", "/keyed", r#"{"id":"u-1","name":"x"}"#);
    assert_eq!((status, body), (201, r#"{"id":"u-1","name":"x"}"#.to_string()));
    assert_eq!(call(&s, "GET", "/keyed/u-1", "").1, r#"{"id":"u-1","name":"x"}"#);
    assert_eq!(call(&s, "POST", "/keyed", r#"{"id":"u-1","name":"dup"}"#).0, 409);
    assert_eq!(call(&s, "POST", "/keyed", r#"{"name":"auto"}"#).1, r#"{"id":1,"name":"auto"}"#);

    let s = server();
    assert_eq!(call(&s, "POST", "/keyed", r#"{"id":1,"name":"taken"}"#).0, 201);
    assert_eq!(call(&s, "POST", "/keyed", r#"{"name":"auto"}"#).1, r#"{"id":2,"name":"auto"}"#);
    assert_eq!(call(&s, "GET", "/keyed/1", "").1, r#"{"id":1,"name":"taken"}"#);
    assert_eq!(call(&s, "GET", "/keyed/2", "").1, r#"{"id":2,"name":"auto"}"#);
}

#[test]
fn metrics_endpoint() {
    let mut s = Server::new(compile(SRC, None).unwrap()).unwrap();
    Arc::get_mut(&mut s).unwrap().metrics_path = Some("/_metrics".to_string());
    call(&s, "GET", "/health", "");
    call(&s, "GET", "/missing", "");
    call(&s, "GET", "/users/999", "");
    let (status, body, ct) = call(&s, "GET", "/_metrics", "");
    assert_eq!((status, ct), (200, JSON));
    let m = velo::value::parse_json(body.as_bytes()).unwrap();
    assert!(matches!(m.get("version"), Value::Str(_)), "{body}");
    assert!(matches!(m.get("requests"), Value::Num(n) if n >= 4.0), "{body}");
    assert!(matches!(m.get("failures"), Value::Num(n) if n == 2.0), "{body}");
    assert!(matches!(m.get("routes"), Value::Num(n) if n > 0.0), "{body}");
    for key in ["bytes_out", "avg_micros", "max_micros", "uptime_ms", "connections"] {
        assert!(matches!(m.get(key), Value::Num(n) if n >= 0.0), "{key} missing in {body}");
    }

    let plain = server();
    assert_eq!(call(&plain, "GET", "/_metrics", "").0, 404);
}

#[test]
fn metrics_break_down_by_route() {
    let mut s = Server::new(compile(SRC, None).unwrap()).unwrap();
    Arc::get_mut(&mut s).unwrap().metrics_path = Some("/_metrics".to_string());
    let hit = |method: &str, path: &str, body: &str| {
        let mut out = Vec::new();
        let (status, _, _, route) =
            s.handle_full(method, path, body.as_bytes(), &[], &mut out, &mut Vec::new());
        s.record_route(route, status, 1);
        status
    };
    assert_eq!(hit("GET", "/health", ""), 200);
    assert_eq!(hit("GET", "/health", ""), 200);
    assert_eq!(hit("POST", "/users", r#"{"name":"a"}"#), 201);
    assert_eq!(hit("GET", "/users/999", ""), 404);
    assert_eq!(hit("GET", "/nowhere", ""), 404);

    let (_, body, _) = call(&s, "GET", "/_metrics", "");
    let m = velo::value::parse_json(body.as_bytes()).unwrap();
    let Value::Arr(paths) = m.get("paths") else { panic!("no breakdown in {body}") };
    let find = |label: &str| {
        paths.iter().find(|p| p.get("route").as_key() == label).unwrap_or_else(|| {
            panic!("{label} missing in {body}");
        })
    };
    assert_eq!(find("GET /health").get("hits").as_key(), "2");
    assert_eq!(find("GET /health").get("failures").as_key(), "0");
    assert_eq!(find("POST /users").get("hits").as_key(), "1");
    assert_eq!(find("GET /users/:id").get("hits").as_key(), "1");
    assert_eq!(find("GET /users/:id").get("failures").as_key(), "1");
    assert!(!paths.iter().any(|p| p.get("route").as_key().contains("nowhere")), "{body}");
    assert!(paths.iter().all(|p| p.get("hits").as_key() != "0"), "{body}");
}

#[test]
fn expect_continue_is_answered() {
    let port = spawn();
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(
        b"POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 15\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n",
    )
    .unwrap();
    let mut buf = [0u8; 128];
    let n = c.read(&mut buf).unwrap();
    let interim = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(interim.starts_with("HTTP/1.1 100 Continue\r\n\r\n"), "{interim}");

    c.write_all(b"{\"name\":\"mark\"}").unwrap();
    let mut rest = String::new();
    c.read_to_string(&mut rest).unwrap();
    assert!(rest.starts_with("HTTP/1.1 201 Created"), "{rest}");
    assert!(rest.ends_with(r#"{"id":1,"name":"mark"}"#), "{rest}");
}

#[test]
fn etag_round_trip() {
    let mut s = Server::new(compile(SRC, None).unwrap()).unwrap();
    Arc::get_mut(&mut s).unwrap().etag = true;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bg = s.clone();
    std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));

    let res = raw(port, b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    let tag = res
        .lines()
        .find(|l| l.starts_with("ETag: "))
        .map(|l| l.trim_start_matches("ETag: ").trim().to_string())
        .unwrap_or_default();
    assert!(tag.starts_with('"') && tag.ends_with('"'), "{res}");

    let req = format!(
        "GET /health HTTP/1.1\r\nHost: x\r\nIf-None-Match: {tag}\r\nConnection: close\r\n\r\n"
    );
    let res = raw(port, req.as_bytes());
    assert!(res.starts_with("HTTP/1.1 304 Not Modified"), "{res}");
    assert!(!res.ends_with("ok"), "{res}");

    let req = "GET /health HTTP/1.1\r\nHost: x\r\nIf-None-Match: \"deadbeef\"\r\nConnection: close\r\n\r\n";
    let res = raw(port, req.as_bytes());
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    assert!(res.ends_with("ok"), "{res}");

    let plain = server();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port2 = listener.local_addr().unwrap().port();
    let bg = plain.clone();
    std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));
    let res = raw(port2, b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(!res.contains("ETag"), "{res}");

    s.shutdown();
    plain.shutdown();
}

#[test]
fn guard_status_override() {
    let s = server();
    assert_eq!(call(&s, "POST", "/validated", r#"{"name":"ok"}"#).0, 201);
    let (status, body, _) = call(&s, "POST", "/validated", r#"{"other":1}"#);
    assert_eq!((status, body), (400, r#"{"error":"invalid body"}"#.to_string()));
    assert_eq!(call(&s, "POST", "/validated", r#"{"name":""}"#).0, 400);

    let mut out = Vec::new();
    let raw = "GET /forbidden HTTP/1.1\r\nX-Key: nope\r\n\r\n";
    let (status, _) = s.handle("GET", "/forbidden", b"", raw.as_bytes(), &mut out);
    assert_eq!(status, 403);

    let mut out = Vec::new();
    let raw = "GET /forbidden HTTP/1.1\r\nX-Key: root\r\n\r\n";
    let (status, _) = s.handle("GET", "/forbidden", b"", raw.as_bytes(), &mut out);
    assert_eq!((status, String::from_utf8(out).unwrap()), (200, "secret".to_string()));

    assert!(compile("GET /a => 1 when 1 else 99", None).is_err());
}

#[test]
fn where_cache_invalidates() {
    let s = server();
    call(&s, "POST", "/users", r#"{"name":"a","team":"red"}"#);
    assert_eq!(call(&s, "GET", "/search?name=a", "").1, r#"[{"id":1,"name":"a","team":"red"}]"#);
    assert_eq!(call(&s, "GET", "/search?name=a", "").1, r#"[{"id":1,"name":"a","team":"red"}]"#);
    call(&s, "POST", "/users", r#"{"name":"a","team":"blue"}"#);
    assert_eq!(
        call(&s, "GET", "/search?name=a", "").1,
        r#"[{"id":1,"name":"a","team":"red"},{"id":2,"name":"a","team":"blue"}]"#
    );
    call(&s, "PUT", "/users/1", r#"{"name":"z"}"#);
    assert_eq!(call(&s, "GET", "/search?name=a", "").1, r#"[{"id":2,"name":"a","team":"blue"}]"#);
    call(&s, "DELETE", "/users/2", "");
    assert_eq!(call(&s, "GET", "/search?name=a", "").1, "[]");
    for i in 0..40 {
        call(&s, "GET", &format!("/search?name=miss{i}"), "");
    }
    assert_eq!(call(&s, "GET", "/search?name=z", "").1, r#"[{"id":1,"name":"z","team":"red"}]"#);
}

#[test]
fn cache_respects_byte_budget() {
    std::env::set_var("VELO_CACHE_BYTES", "200");
    let s = server();
    for i in 0..40 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"n{i}","team":"t{i}"}}"#));
    }
    let big = call(&s, "GET", "/users", "").1;
    assert!(big.len() > 200, "{}", big.len());
    assert_eq!(call(&s, "GET", "/users", "").1, big);

    let one = call(&s, "GET", "/search?name=n3", "").1;
    assert_eq!(one, r#"[{"id":4,"name":"n3","team":"t3"}]"#);
    assert_eq!(call(&s, "GET", "/search?name=n3", "").1, one);
    assert_eq!(call(&s, "GET", "/sorted", "").1.matches(r#""id""#).count(), 40);
    assert_eq!(call(&s, "GET", "/search?name=n3", "").1, one);
    std::env::remove_var("VELO_CACHE_BYTES");
}

#[test]
fn openapi_document() {
    let prog = compile(SRC, None).unwrap();
    let doc = velo::openapi::document(&prog, "test api", "9.9.9");
    let text = String::from_utf8(doc.clone()).unwrap();
    let v = velo::value::parse_json(&doc).expect(&text);

    assert_eq!(v.get("openapi").as_key(), "3.0.3");
    assert_eq!(v.get("info").get("title").as_key(), "test api");
    assert_eq!(v.get("info").get("version").as_key(), "9.9.9");

    let paths = v.get("paths");
    let by_id = paths.get("/users/{id}");
    assert!(matches!(by_id.get("get"), Value::Obj(_)), "{text}");
    assert_eq!(by_id.get("get").get("operationId").as_key(), "get_users_by_id");
    assert!(text.contains(r#""name":"id","in":"path","required":true"#), "{text}");
    assert!(matches!(by_id.get("get").get("responses").get("404"), Value::Obj(_)), "{text}");

    let post = paths.get("/users").get("post");
    assert!(matches!(post.get("requestBody"), Value::Obj(_)), "{text}");
    assert!(matches!(post.get("responses").get("201"), Value::Obj(_)), "{text}");

    assert!(text.contains(r#""name":"limit","in":"query""#), "{text}");
    assert!(text.contains(r#""name":"x-team","in":"header""#), "{text}");
    assert!(text.contains(r#""/gone/{id}""#), "{text}");
}

#[test]
fn autosave_writes_and_keeps_up() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("velo-autosave-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = velo::Store::new();
    let s = Server::new(compile(SRC, Some(store.clone())).unwrap()).unwrap();
    store.autosave(path.clone(), Duration::from_millis(20));

    call(&s, "POST", "/users", r#"{"name":"first"}"#);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(raw) = std::fs::read(&path) {
            if String::from_utf8_lossy(&raw).contains("first") {
                break;
            }
        }
        assert!(std::time::Instant::now() < deadline, "autosave never wrote {path:?}");
        std::thread::sleep(Duration::from_millis(20));
    }

    call(&s, "POST", "/users", r#"{"name":"second"}"#);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(raw) = std::fs::read(&path) {
            if String::from_utf8_lossy(&raw).contains("second") {
                break;
            }
        }
        assert!(std::time::Instant::now() < deadline, "second write never saved");
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rate_limit_counts_per_client() {
    let mut s = Server::new(compile(SRC, None).unwrap()).unwrap();
    {
        let srv = Arc::get_mut(&mut s).unwrap();
        srv.rate = 3;
        srv.real_ip_header = Some("x-real-ip".to_string());
    }
    for _ in 0..3 {
        assert!(s.allow("1.2.3.4"));
    }
    assert!(!s.allow("1.2.3.4"));
    assert!(s.allow("5.6.7.8"));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bg = s.clone();
    std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));

    let hit = |ip: &str| {
        let req = format!(
            "GET /health HTTP/1.1\r\nHost: x\r\nX-Real-Ip: {ip}\r\nConnection: close\r\n\r\n"
        );
        raw(port, req.as_bytes())
    };
    let mut codes = Vec::new();
    for _ in 0..5 {
        codes.push(hit("9.9.9.9").lines().next().unwrap_or_default().to_string());
    }
    assert!(codes.iter().any(|c| c.contains("429 Too Many Requests")), "{codes:?}");
    assert!(codes.iter().filter(|c| c.contains("200 OK")).count() >= 3, "{codes:?}");
    assert!(hit("8.8.8.8").ends_with("ok"));

    let open = server();
    assert!(open.allow("1.2.3.4"));
    s.shutdown();
}

#[test]
fn openapi_builtin_serves_the_document() {
    let src =
        "GET /health => \"ok\"\nGET /docs => openapi()\nGET /users/:id => db.users.find(id)\n";
    let prog = compile(src, None).unwrap();
    let docs = prog.routes.iter().find(|r| r.pattern == "/docs").unwrap();
    assert!(docs.konst.is_some());
    assert_eq!(docs.const_ctype, JSON);

    let s = Server::new(prog).unwrap();
    let (status, body, ct) = call(&s, "GET", "/docs", "");
    assert_eq!((status, ct), (200, JSON));
    let v = velo::value::parse_json(body.as_bytes()).expect(&body);
    assert_eq!(v.get("openapi").as_key(), "3.0.3");
    assert!(matches!(v.get("paths").get("/users/{id}"), Value::Obj(_)), "{body}");
    assert!(matches!(v.get("paths").get("/docs"), Value::Obj(_)), "{body}");
}

#[test]
fn deep_pipelining_returns_every_response() {
    let s = server();
    for i in 0..60 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"user-{i}"}}"#));
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bg = s.clone();
    std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut batch = String::new();
    for _ in 0..99 {
        batch.push_str("GET /users HTTP/1.1\r\nHost: x\r\n\r\n");
    }
    batch.push_str("GET /users HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    c.write_all(batch.as_bytes()).unwrap();

    let mut out = Vec::new();
    c.read_to_end(&mut out).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 100, "{} bytes", out.len());
    assert_eq!(text.matches("user-59").count(), 100);
    s.shutdown();
}

#[test]
fn search_matches_substrings() {
    let s = server();
    call(&s, "POST", "/users", r#"{"name":"Alice Smith"}"#);
    call(&s, "POST", "/users", r#"{"name":"bob smithers"}"#);
    call(&s, "POST", "/users", r#"{"name":"carol"}"#);

    let hits = call(&s, "GET", "/find?q=smith", "").1;
    assert!(hits.contains("Alice Smith"), "{hits}");
    assert!(hits.contains("bob smithers"), "{hits}");
    assert!(!hits.contains("carol"), "{hits}");

    assert_eq!(call(&s, "GET", "/find?q=SMITH", "").1, hits);
    assert_eq!(call(&s, "GET", "/find?q=zzz", "").1, "[]");
    assert_eq!(call(&s, "GET", "/find?q=carol", "").1.matches(r#""id""#).count(), 1);

    assert_eq!(call(&s, "GET", "/find?q=smith", "").1, hits);
    call(&s, "POST", "/users", r#"{"name":"dave smith"}"#);
    assert!(call(&s, "GET", "/find?q=smith", "").1.contains("dave smith"));
}

#[test]
fn concurrent_reads_and_writes_stay_consistent() {
    let s = server();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let readers: Vec<_> = ["/users", "/sorted", "/search?name=a", "/find?q=a", "/stats"]
        .iter()
        .map(|path| {
            let (s, stop, path) = (s.clone(), stop.clone(), path.to_string());
            std::thread::spawn(move || {
                let mut seen = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let mut out = Vec::new();
                    let (status, _) = s.dispatch("GET", &path, b"", &mut out);
                    assert_eq!(status, 200, "{path}");
                    assert!(velo::value::parse_json(&out).is_ok(), "{path}");
                    seen += 1;
                }
                seen
            })
        })
        .collect();

    let writers: Vec<_> = (0..4)
        .map(|w| {
            let s = s.clone();
            std::thread::spawn(move || {
                for i in 0..200 {
                    let mut out = Vec::new();
                    let body = format!(r#"{{"name":"a","w":{w},"i":{i}}}"#);
                    let (status, _) = s.dispatch("POST", "/users", body.as_bytes(), &mut out);
                    assert_eq!(status, 201);
                }
            })
        })
        .collect();

    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let reads: u64 = readers.into_iter().map(|r| r.join().unwrap()).sum();
    assert!(reads > 0);

    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":800}"#);
    let all = call(&s, "GET", "/users", "").1;
    assert_eq!(all.matches(r#""name":"a""#).count(), 800);
    assert_eq!(call(&s, "GET", "/search?name=a", "").1, all);
    assert_eq!(call(&s, "GET", "/sorted", "").1.matches(r#""id""#).count(), 800);
}

#[test]
fn aggregations() {
    let s = server();
    assert_eq!(
        call(&s, "GET", "/totals", "").1,
        r#"{"n":0,"sum":0,"avg":null,"lo":null,"hi":null}"#
    );
    call(&s, "POST", "/users", r#"{"name":"a","score":10}"#);
    call(&s, "POST", "/users", r#"{"name":"b","score":5}"#);
    call(&s, "POST", "/users", r#"{"name":"c","score":6}"#);
    call(&s, "POST", "/users", r#"{"name":"d"}"#);
    assert_eq!(call(&s, "GET", "/totals", "").1, r#"{"n":4,"sum":21,"avg":7,"lo":5,"hi":10}"#);
    assert_eq!(call(&s, "GET", "/totals", "").1, call(&s, "GET", "/totals", "").1);
    call(&s, "POST", "/users", r#"{"name":"e","score":100}"#);
    assert_eq!(
        call(&s, "GET", "/totals", "").1,
        r#"{"n":5,"sum":121,"avg":30.25,"lo":5,"hi":100}"#
    );
    call(&s, "DELETE", "/users/5", "");
    assert_eq!(call(&s, "GET", "/totals", "").1, r#"{"n":4,"sum":21,"avg":7,"lo":5,"hi":10}"#);
}

#[test]
fn derived_results_hit_the_cache() {
    let store = velo::Store::new();
    let s = Server::new(compile(SRC, Some(store.clone())).unwrap()).unwrap();
    let users = store.collection("users");
    for i in 0..20 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"n{i}","score":{i}}}"#));
    }
    let reads = |s: &Arc<Server>| {
        call(s, "GET", "/sorted", "");
        call(s, "GET", "/search?name=n3", "");
        call(s, "GET", "/find?q=n1", "");
        call(s, "GET", "/totals", "");
        call(s, "GET", "/users", "");
    };

    reads(&s);
    let (_, warm) = users.cache_stats();
    for _ in 0..5 {
        reads(&s);
    }
    let (_, after) = users.cache_stats();
    assert_eq!(after, warm, "repeated reads must not rebuild anything");

    call(&s, "POST", "/users", r#"{"name":"new"}"#);
    reads(&s);
    let (_, rebuilt) = users.cache_stats();
    assert!(rebuilt > after, "a write must invalidate the derived results");
    reads(&s);
    assert_eq!(users.cache_stats().1, rebuilt, "second read after a write must be cached");
}

#[test]
fn local_cache_respects_its_budget() {
    std::env::set_var("VELO_LOCAL_CACHE_BYTES", "300");
    let store = velo::Store::new();
    let s = Server::new(compile(SRC, Some(store.clone())).unwrap()).unwrap();
    let users = store.collection("users");
    for i in 0..40 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"n{i}"}}"#));
    }
    let big = call(&s, "GET", "/users", "").1;
    assert!(big.len() > 300);
    assert_eq!(call(&s, "GET", "/users", "").1, big);

    let small = call(&s, "GET", "/search?name=n7", "").1;
    assert_eq!(small, r#"[{"id":8,"name":"n7"}]"#);
    let (_, before) = users.cache_stats();
    assert_eq!(call(&s, "GET", "/search?name=n7", "").1, small);
    let (_, after) = users.cache_stats();
    assert_eq!(after, before, "small results still cache locally");
    std::env::remove_var("VELO_LOCAL_CACHE_BYTES");
}

#[test]
fn incremental_list_matches_a_rebuild() {
    let a = server();
    let b = server();
    for i in 0..25 {
        let body = format!(r#"{{"name":"n{i}","team":"t{}"}}"#, i % 3);
        call(&a, "POST", "/users", &body);
        call(&b, "POST", "/users", &body);
        call(&a, "GET", "/users", "");
    }
    assert_eq!(call(&a, "GET", "/users", "").1, call(&b, "GET", "/users", "").1);

    call(&a, "DELETE", "/users/3", "");
    call(&b, "DELETE", "/users/3", "");
    call(&a, "GET", "/users", "");
    call(&a, "POST", "/users", r#"{"name":"after"}"#);
    call(&b, "POST", "/users", r#"{"name":"after"}"#);
    assert_eq!(call(&a, "GET", "/users", "").1, call(&b, "GET", "/users", "").1);

    call(&a, "PUT", "/users/2", r#"{"name":"edited"}"#);
    call(&b, "PUT", "/users/2", r#"{"name":"edited"}"#);
    let listed = call(&a, "GET", "/users", "").1;
    assert_eq!(listed, call(&b, "GET", "/users", "").1);
    assert!(listed.contains("edited"), "{listed}");
    assert!(!listed.contains(r#""name":"n1""#), "{listed}");

    let fresh = server();
    assert_eq!(call(&fresh, "GET", "/users", "").1, "[]");
    call(&fresh, "GET", "/users", "");
    call(&fresh, "POST", "/users", r#"{"name":"only"}"#);
    assert_eq!(call(&fresh, "GET", "/users", "").1, r#"[{"id":1,"name":"only"}]"#);
}

#[test]
fn write_heavy_bursts_do_not_rebuild_the_list() {
    let store = velo::Store::new();
    let s = Server::new(compile(SRC, Some(store.clone())).unwrap()).unwrap();
    let users = store.collection("users");
    call(&s, "GET", "/users", "");
    for i in 0..50 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"w{i}"}}"#));
    }
    let listed = call(&s, "GET", "/users", "").1;
    assert_eq!(listed.matches(r#""name""#).count(), 50);

    for i in 0..20 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"r{i}"}}"#));
        let seen = call(&s, "GET", "/users", "").1;
        assert_eq!(seen.matches(r#""name""#).count(), 51 + i);
    }
    let (_, misses) = users.cache_stats();
    call(&s, "GET", "/users", "");
    assert_eq!(users.cache_stats().1, misses, "list reads must not touch the shared cache");
}

#[test]
fn deletes_keep_order_and_indexes_valid() {
    let s = server();
    for i in 0..12 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"n{i}","team":"t{}"}}"#, i % 2));
    }
    for id in [2, 4, 6, 8] {
        assert_eq!(call(&s, "DELETE", &format!("/users/{id}"), "").1, r#"{"deleted":true}"#);
    }
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":8}"#);

    let listed = call(&s, "GET", "/users", "").1;
    let ids: Vec<&str> = listed.matches(r#""id":"#).map(|_| "").collect();
    assert_eq!(ids.len(), 8, "{listed}");
    assert!(listed.starts_with(r#"[{"id":1,"name":"n0""#), "{listed}");
    assert!(!listed.contains(r#""id":4"#), "{listed}");

    assert_eq!(call(&s, "GET", "/users/9", "").1, r#"{"id":9,"name":"n8","team":"t0"}"#);
    assert_eq!(call(&s, "GET", "/users/4", "").0, 404);
    assert_eq!(
        call(&s, "PUT", "/users/9", r#"{"name":"moved"}"#).1,
        r#"{"id":9,"name":"moved","team":"t0"}"#
    );
    assert_eq!(call(&s, "GET", "/users/9", "").1, r#"{"id":9,"name":"moved","team":"t0"}"#);

    assert_eq!(call(&s, "GET", "/paged?offset=0&limit=3", "").1.matches(r#""id""#).count(), 3);
    assert_eq!(call(&s, "GET", "/paged?offset=6&limit=5", "").1.matches(r#""id""#).count(), 2);
    assert_eq!(call(&s, "GET", "/sorted", "").1.matches(r#""id""#).count(), 8);
    assert_eq!(call(&s, "GET", "/search?name=n0", "").1, r#"[{"id":1,"name":"n0","team":"t0"}]"#);

    for id in [1, 3, 5, 7, 9, 10, 11, 12] {
        call(&s, "DELETE", &format!("/users/{id}"), "");
    }
    assert_eq!(call(&s, "GET", "/users", "").1, "[]");
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":0}"#);
    let (_, body, _) = call(&s, "POST", "/users", r#"{"name":"fresh"}"#);
    assert_eq!(body, r#"{"id":13,"name":"fresh"}"#);
    assert_eq!(call(&s, "GET", "/users/13", "").1, body);
}

#[test]
fn drip_fed_headers_still_parse() {
    let port = spawn();
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    c.set_nodelay(true).unwrap();

    let mut req = String::from("GET /health HTTP/1.1\r\nHost: x\r\n");
    for i in 0..60 {
        req.push_str(&format!("X-Pad-{i}: {}\r\n", "p".repeat(60)));
    }
    req.push_str("Connection: close\r\n\r\n");
    assert!(req.len() > 3500, "{}", req.len());

    let started = std::time::Instant::now();
    for chunk in req.as_bytes().chunks(7) {
        c.write_all(chunk).unwrap();
    }
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 200 OK"), "{out}");
    assert!(out.ends_with("ok"), "{out}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn form_encoded_bodies_are_accepted() {
    let s = server();
    let (status, body, _) = call(&s, "POST", "/users", "name=mark&team=red");
    assert_eq!((status, body), (201, r#"{"id":1,"name":"mark","team":"red"}"#.to_string()));

    let (_, body, _) = call(&s, "POST", "/users", "name=a+b&note=%E0%B9%84%E0%B8%97%E0%B8%A2");
    assert_eq!(body, "{\"id\":2,\"name\":\"a b\",\"note\":\"ไทย\"}");

    assert_eq!(call(&s, "POST", "/users", r#"{"name":"json"}"#).0, 201);
    assert_eq!(call(&s, "POST", "/users", "not a body").0, 400);
    assert_eq!(call(&s, "POST", "/users", "{oops").0, 400);
    assert_eq!(call(&s, "POST", "/users", "=novalue").0, 400);
    assert_eq!(call(&s, "POST", "/name", "name=fromform"), (201, "fromform".into(), TEXT));
}

#[test]
fn extra_headers_are_parsed_and_validated() {
    let render =
        |spec: &str| String::from_utf8(velo::http::extra_headers(Some(spec.into()))).unwrap();
    assert_eq!(render("X-A: 1"), "X-A: 1\r\n");
    assert_eq!(render("X-A: 1; X-B: two"), "X-A: 1\r\nX-B: two\r\n");
    assert_eq!(render("X-A: 1\nX-B: two\n"), "X-A: 1\r\nX-B: two\r\n");
    assert_eq!(
        render("Cache-Control: public, max-age=60"),
        "Cache-Control: public, max-age=60\r\n"
    );
    assert_eq!(render("no colon here"), "");
    assert_eq!(render("Bad Name: 1"), "");
    assert_eq!(render("X-A: with\rinjection"), "");
    assert_eq!(render(""), "");
    assert_eq!(String::from_utf8(velo::http::extra_headers(None)).unwrap(), "");
}

#[test]
fn arithmetic_and_comparisons() {
    let s = server();
    assert_eq!(
        call(&s, "GET", "/math", "").1,
        r#"{"sum":14,"grouped":20,"div":2.5,"neg":-7,"join":"ab"}"#
    );
    assert_eq!(
        call(&s, "GET", "/calc/21", "").1,
        r#"{"n":"21","doubled":42,"next":22,"big":true}"#
    );
    assert_eq!(call(&s, "GET", "/calc/3", "").1, r#"{"n":"3","doubled":6,"next":4,"big":false}"#);
    assert_eq!(
        call(&s, "GET", "/calc/abc", "").1,
        r#"{"n":"abc","doubled":null,"next":null,"big":false}"#
    );

    let math = compile("GET /a => { v: 6 * 7 }", None).unwrap();
    assert_eq!(math.routes[0].konst.as_deref(), Some(br#"{"v":42}"#.as_slice()));

    for _ in 0..3 {
        call(&s, "POST", "/users", r#"{"name":"u"}"#);
    }
    assert_eq!(call(&s, "GET", "/limited?limit=2", "").1.matches(r#""id""#).count(), 2);
    assert_eq!(call(&s, "GET", "/limited?limit=500", "").0, 400);
    assert_eq!(call(&s, "GET", "/math", "").2, JSON);
    assert!(compile("GET /a => 1 +", None).is_err());
    assert!(compile("GET /a => (1 + 2", None).is_err());
}

#[test]
fn comparison_type_rules() {
    let cases = [
        ("GET /a => 2 < 10", "true"),
        ("GET /a => \"2\" < \"10\"", "true"),
        ("GET /a => \"b\" < \"a\"", "false"),
        ("GET /a => \"abc\" > 10", "false"),
        ("GET /a => null > 1", "false"),
        ("GET /a => 1 <= 1", "true"),
        ("GET /a => 3 >= 4", "false"),
        ("GET /a => \"7\" * \"6\"", "42"),
        ("GET /a => \"x\" * 2", "null"),
        ("GET /a => 5 / 0", "null"),
        ("GET /a => 1 + 2 + 3", "6"),
        ("GET /a => 10 - 3 - 2", "5"),
        ("GET /a => 2 * 3 + 4 * 5", "26"),
        ("GET /a => \"a\" + \"b\" + \"c\"", "abc"),
    ];
    for (src, want) in cases {
        let prog = compile(src, None).unwrap_or_else(|e| panic!("{src}: {e}"));
        let s = Server::new(prog).unwrap();
        assert_eq!(call(&s, "GET", "/a", "").1, want, "{src}");
    }
}

#[test]
fn boolean_guards() {
    let s = server();
    let call_with = |path: &str, headers: &str| {
        let raw = format!("GET {path} HTTP/1.1\r\n{headers}\r\n");
        let mut out = Vec::new();
        let (status, _) = s.handle("GET", path, b"", raw.as_bytes(), &mut out);
        (status, String::from_utf8(out).unwrap())
    };
    assert_eq!(call_with("/both", "X-Key: a\r\nX-Team: b\r\n"), (200, "both".into()));
    assert_eq!(call_with("/both", "X-Key: a\r\n").0, 400);
    assert_eq!(call_with("/both", "X-Team: b\r\n").0, 400);
    assert_eq!(call_with("/both", "").0, 400);

    assert_eq!(call_with("/either", "X-Key: a\r\n"), (200, "either".into()));
    assert_eq!(call_with("/either", "X-Team: b\r\n"), (200, "either".into()));
    assert_eq!(call_with("/either", "").0, 401);

    let prog = compile("GET /a => 1 when 1 == 1 and 2 < 3", None).unwrap();
    assert!(prog.routes[0].guard.is_some());
    let s2 = Server::new(prog).unwrap();
    assert_eq!(call(&s2, "GET", "/a", "").0, 200);
    let s3 = Server::new(compile("GET /a => 1 when 1 == 2 or 3 > 9", None).unwrap()).unwrap();
    assert_eq!(call(&s3, "GET", "/a", "").0, 401);
}

#[test]
fn shutdown_drains_in_flight_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = server();
    let bg = s.clone();
    let h = std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));

    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut buf = [0u8; 512];
    let n = c.read(&mut buf).unwrap();
    let first = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(first.contains("Connection: keep-alive"), "{first}");

    s.shutdown();
    c.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut rest = String::new();
    c.read_to_string(&mut rest).unwrap();
    assert!(rest.starts_with("HTTP/1.1 200 OK"), "in-flight request dropped: {rest:?}");
    assert!(rest.contains("Connection: close"), "{rest}");
    assert!(rest.ends_with("ok"), "{rest}");

    let started = std::time::Instant::now();
    h.join().unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err() || s.stopping());
}

#[test]
fn socket_activation_is_detected() {
    use velo::socket::activation_fd;
    let s = |v: &str| Some(v.to_string());
    assert_eq!(activation_fd(42, s("42"), s("1")), Some(3));
    assert_eq!(activation_fd(42, s("42"), s("2")), Some(3));
    assert_eq!(activation_fd(42, s("41"), s("1")), None, "another process owns the fds");
    assert_eq!(activation_fd(42, s("42"), s("0")), None);
    assert_eq!(activation_fd(42, None, s("1")), None);
    assert_eq!(activation_fd(42, s("42"), None), None);
    assert_eq!(activation_fd(42, s("nope"), s("1")), None);
}

#[test]
fn upsert_creates_or_merges() {
    let s = server();
    let (status, body, _) = call(&s, "PUT", "/put/7", r#"{"name":"seven"}"#);
    assert_eq!((status, body), (200, r#"{"id":7,"name":"seven"}"#.to_string()));
    assert_eq!(call(&s, "GET", "/users/7", "").1, r#"{"id":7,"name":"seven"}"#);
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":1}"#);

    let (_, body, _) = call(&s, "PUT", "/put/7", r#"{"team":"red"}"#);
    assert_eq!(body, r#"{"id":7,"name":"seven","team":"red"}"#);
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":1}"#);

    let (_, body, _) = call(&s, "PUT", "/keyed/abc", r#"{"name":"letters"}"#);
    assert_eq!(body, r#"{"id":"abc","name":"letters"}"#);
    assert_eq!(call(&s, "GET", "/keyed/abc", "").1, r#"{"id":"abc","name":"letters"}"#);

    let (_, body, _) = call(&s, "POST", "/users", r#"{"name":"auto"}"#);
    assert_eq!(body, r#"{"id":1,"name":"auto"}"#);
    assert_eq!(call(&s, "GET", "/users", "").1.matches(r#""id""#).count(), 2);
    assert_eq!(call(&s, "PUT", "/put/7", "").0, 400);
}

#[test]
fn bulk_deletes() {
    let s = server();
    for i in 0..6 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"n{i}","team":"t{}"}}"#, i % 3));
    }
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":6}"#);

    assert_eq!(call(&s, "DELETE", "/team/t1", "").1, r#"{"deleted":2}"#);
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":4}"#);
    assert_eq!(call(&s, "GET", "/users/2", "").0, 404);
    assert_eq!(call(&s, "GET", "/users/1", "").1, r#"{"id":1,"name":"n0","team":"t0"}"#);
    assert_eq!(call(&s, "DELETE", "/team/t1", "").1, r#"{"deleted":0}"#);
    assert_eq!(call(&s, "GET", "/users", "").1.matches(r#""id""#).count(), 4);

    assert_eq!(call(&s, "DELETE", "/wipe", "").1, r#"{"deleted":4}"#);
    assert_eq!(call(&s, "GET", "/users", "").1, "[]");
    assert_eq!(call(&s, "DELETE", "/wipe", "").1, r#"{"deleted":0}"#);
    let (_, body, _) = call(&s, "POST", "/users", r#"{"name":"fresh"}"#);
    assert_eq!(body, r#"{"id":1,"name":"fresh"}"#);
    assert_eq!(call(&s, "GET", "/sorted", "").1, r#"[{"id":1,"name":"fresh"}]"#);
}

#[test]
fn protocol_edge_cases() {
    let port = spawn();
    let ask = |req: &str| raw(port, req.as_bytes());

    let res = ask("GET /health HTTP/1.0\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    assert!(res.contains("Connection: close"), "1.0 should close by default: {res}");

    let res = ask("GET /health HTTP/1.0\r\nConnection: keep-alive\r\nHost: x\r\n\r\n");
    assert!(res.contains("Connection: keep-alive"), "{res}");

    let res = ask("GET /health HTTP/1.1\r\nConnection: close\r\n\r\n");
    assert!(res.ends_with("ok"), "missing Host is still served: {res}");

    let res = ask("get /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 405"), "methods are case sensitive: {res}");

    let res = ask("GET http://x/health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 404"), "absolute-form targets are not routed: {res}");

    let res = ask("OPTIONS * HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 404"), "{res}");
}

const CHAIN_SRC: &str = r#"
POST /users        => db.users.create(body)
GET  /team         => db.users.where("team", query.t).order("name")
GET  /page         => db.users.where("team", query.t).order("-score").page(query.o, query.n)
GET  /howmany      => db.users.where("team", query.t).count()
GET  /score        => db.users.where("team", query.t).sum("score")
GET  /best         => db.users.order("-score").first()
GET  /named        => db.users.order("name").first("team", query.t)
GET  /hunt         => db.users.search("name", query.q).page(0, 2)
GET  /topname      => db.users.order("-score").first().name
GET  /one/:id      => db.users.find(id).name
GET  /every        => db.users.all().where("team", query.t).count()
GET  /cards        => db.users.select("name", "score")
GET  /roster       => db.users.where("team", query.t).order("name").select("name")
GET  /slim         => db.users.select("name", "nope")
GET  /gate         => "in" when db.users.where("team", query.t).count()
GET  /gatesum      => "in" when db.users.where("team", query.t).sum("score")
"#;

fn chain_server() -> Arc<Server> {
    let s = Server::new(compile(CHAIN_SRC, None).unwrap()).unwrap();
    for (name, team, score) in [
        ("ann", "red", 5),
        ("bob", "blue", 9),
        ("cid", "red", 7),
        ("dan", "red", 1),
        ("eve", "blue", 3),
    ] {
        let body = format!(r#"{{"name":"{name}","team":"{team}","score":{score}}}"#);
        assert_eq!(call(&s, "POST", "/users", &body).0, 201);
    }
    s
}

const INDEX_SRC: &str = r#"
POST   /users        => db.users.create(body)
PUT    /users/:id    => db.users.upsert(id, body)
DELETE /users/:id    => db.users.delete(id) : 204
GET    /count        => db.users.where("team", query.t).count()
GET    /rows         => db.users.where("team", query.t).select("name")
GET    /first        => db.users.where("team", query.t).order("name").first()
GET    /sorted       => db.users.where("team", query.t).order("-score").page(0, 3).select("name")
GET    /narrow       => db.users.where("team", query.t).where("score", ">=", query.lo).count()
GET    /blank        => db.users.where("nothing", "").count()
"#;

#[test]
fn an_indexed_filter_answers_what_a_scan_would() {
    let s = Server::new(compile(INDEX_SRC, None).unwrap()).unwrap();
    let teams = ["red", "blue", "green"];
    for i in 0..1500 {
        let body = format!(
            r#"{{"name":"u{i:04}","team":"{}","score":{}}}"#,
            teams[i % teams.len()],
            i % 100
        );
        assert_eq!(call(&s, "POST", "/users", &body).0, 201);
    }
    let count = |team: &str| call(&s, "GET", &format!("/count?t={team}"), "").1;
    assert_eq!(count("red"), "500");
    assert_eq!(count("blue"), "500");
    assert_eq!(count("green"), "500");
    assert_eq!(count("none"), "0");
    assert_eq!(call(&s, "GET", "/blank", "").1, "1500");
    assert_eq!(call(&s, "GET", "/narrow?t=red&lo=50", "").1, "250");
    assert_eq!(names(&call(&s, "GET", "/rows?t=red", "").1).len(), 500);
    let first = call(&s, "GET", "/first?t=blue", "").1;
    assert!(first.contains(r#""name":"u0001""#), "{first}");
    assert_eq!(names(&call(&s, "GET", "/sorted?t=red", "").1).len(), 3);

    assert_eq!(call(&s, "POST", "/users", r#"{"name":"zz","team":"red"}"#).0, 201);
    assert_eq!(count("red"), "501");
    assert_eq!(count("blue"), "500");

    assert_eq!(call(&s, "PUT", "/users/1", r#"{"team":"blue"}"#).0, 200);
    assert_eq!(count("red"), "500");
    assert_eq!(count("blue"), "501");

    assert_eq!(call(&s, "DELETE", "/users/2", "").0, 204);
    assert_eq!(count("blue"), "500");
    assert_eq!(count("red"), "500");
    assert_eq!(count("green"), "500");
    assert_eq!(names(&call(&s, "GET", "/rows?t=green", "").1).len(), 500);
}

#[test]
fn an_index_survives_many_fields_and_a_rebuild() {
    let s = Server::new(compile(INDEX_SRC, None).unwrap()).unwrap();
    for i in 0..900 {
        let body = format!(r#"{{"name":"u{i:04}","team":"t{}","score":{}}}"#, i % 7, i % 5);
        assert_eq!(call(&s, "POST", "/users", &body).0, 201);
    }
    let mut want = [0i64; 7];
    for i in 0..900 {
        want[i % 7] += 1;
    }
    for round in 0..3 {
        for (t, expected) in want.iter().enumerate() {
            let got = call(&s, "GET", &format!("/count?t=t{t}"), "").1;
            assert_eq!(got, expected.to_string(), "round {round} team t{t}");
        }
        let doomed = round + 1;
        assert_eq!(call(&s, "DELETE", &format!("/users/{doomed}"), "").0, 204);
        want[(doomed - 1) % 7] -= 1;
        assert_eq!(call(&s, "POST", "/users", r#"{"name":"x","team":"t0"}"#).0, 201);
        want[0] += 1;
    }
    let total: i64 = (0..7)
        .map(|t| call(&s, "GET", &format!("/count?t=t{t}"), "").1.parse::<i64>().unwrap())
        .sum();
    assert_eq!(total, want.iter().sum::<i64>());
}

fn names(body: &str) -> Vec<String> {
    match parse_json(body.as_bytes()).unwrap() {
        Value::Arr(rows) => rows.iter().map(|r| r.get("name").as_key()).collect(),
        other => panic!("not a list: {other:?}"),
    }
}

#[test]
fn chain_filters_then_sorts_and_pages() {
    let s = chain_server();
    assert_eq!(names(&call(&s, "GET", "/team?t=red", "").1), ["ann", "cid", "dan"]);
    assert_eq!(names(&call(&s, "GET", "/page?t=red&o=0&n=2", "").1), ["cid", "ann"]);
    assert_eq!(names(&call(&s, "GET", "/page?t=red&o=1&n=2", "").1), ["ann", "dan"]);
    assert_eq!(names(&call(&s, "GET", "/page?t=red&o=9&n=2", "").1), [] as [String; 0]);
    assert_eq!(names(&call(&s, "GET", "/hunt?q=n", "").1), ["ann", "dan"]);
}

#[test]
fn chain_terminals() {
    let s = chain_server();
    assert_eq!(call(&s, "GET", "/howmany?t=red", "").1, "3");
    assert_eq!(call(&s, "GET", "/howmany?t=none", "").1, "0");
    assert_eq!(call(&s, "GET", "/every?t=blue", "").1, "2");
    assert_eq!(call(&s, "GET", "/score?t=red", "").1, "13");
    assert_eq!(call(&s, "GET", "/score?t=none", "").1, "0");
    assert_eq!(call(&s, "GET", "/best", "").1, r#"{"id":2,"name":"bob","team":"blue","score":9}"#);
    assert_eq!(
        call(&s, "GET", "/named?t=red", "").1,
        r#"{"id":1,"name":"ann","team":"red","score":5}"#
    );
    assert_eq!(call(&s, "GET", "/named?t=green", "").0, 404);
}

#[test]
fn a_guard_reads_a_counted_chain_as_a_number() {
    let s = chain_server();
    assert_eq!(call(&s, "GET", "/gate?t=red", "").0, 200);
    assert_eq!(call(&s, "GET", "/gate?t=none", "").0, 401);
    assert_eq!(call(&s, "GET", "/gatesum?t=blue", "").0, 200);
    assert_eq!(call(&s, "GET", "/gatesum?t=none", "").0, 401);
}

#[test]
fn select_keeps_only_the_named_fields() {
    let s = chain_server();
    assert_eq!(
        call(&s, "GET", "/cards", "").1,
        r#"[{"name":"ann","score":5},{"name":"bob","score":9},{"name":"cid","score":7},{"name":"dan","score":1},{"name":"eve","score":3}]"#
    );
    assert_eq!(
        call(&s, "GET", "/roster?t=red", "").1,
        r#"[{"name":"ann"},{"name":"cid"},{"name":"dan"}]"#
    );
    assert_eq!(call(&s, "GET", "/roster?t=none", "").1, "[]");
    assert_eq!(
        call(&s, "GET", "/slim", "").1,
        r#"[{"name":"ann"},{"name":"bob"},{"name":"cid"},{"name":"dan"},{"name":"eve"}]"#
    );
}

#[test]
fn select_sees_later_writes_and_does_not_collide() {
    let s = chain_server();
    assert_eq!(call(&s, "GET", "/roster?t=blue", "").1, r#"[{"name":"bob"},{"name":"eve"}]"#);
    assert_eq!(call(&s, "GET", "/cards", "").1.matches("\"name\"").count(), 5);
    assert_eq!(call(&s, "POST", "/users", r#"{"name":"fay","team":"blue","score":2}"#).0, 201);
    assert_eq!(
        call(&s, "GET", "/roster?t=blue", "").1,
        r#"[{"name":"bob"},{"name":"eve"},{"name":"fay"}]"#
    );
    assert_eq!(call(&s, "GET", "/cards", "").1.matches("\"name\"").count(), 6);
}

#[test]
fn select_needs_a_field() {
    let err = compile("GET /x => db.users.select()\n", None).err().unwrap();
    assert!(err.contains("expects 1 argument"), "{err}");
    let err = compile("GET /x => db.users.select(\"a\").count()\n", None).err().unwrap();
    assert!(err.contains("nothing can follow"), "{err}");
}

#[test]
fn chain_reads_are_invalidated_by_writes() {
    let s = chain_server();
    assert_eq!(call(&s, "GET", "/howmany?t=red", "").1, "3");
    assert_eq!(names(&call(&s, "GET", "/team?t=red", "").1), ["ann", "cid", "dan"]);
    assert_eq!(call(&s, "POST", "/users", r#"{"name":"abe","team":"red","score":4}"#).0, 201);
    assert_eq!(call(&s, "GET", "/howmany?t=red", "").1, "4");
    assert_eq!(names(&call(&s, "GET", "/team?t=red", "").1), ["abe", "ann", "cid", "dan"]);
    assert_eq!(call(&s, "GET", "/score?t=red", "").1, "17");
}

#[test]
fn chain_cache_keys_do_not_collide() {
    let s = Server::new(compile("GET /w => db.k.where(query.f, query.v).count()", None).unwrap())
        .unwrap();
    let col = s.store.collection("k");
    for v in [r#"{"a":"1:x","b":"y"}"#, r#"{"a":"1","b":":xy"}"#] {
        col.create(parse_json(v.as_bytes()).unwrap(), &[]).unwrap();
    }
    assert_eq!(call(&s, "GET", "/w?f=a&v=1:x", "").1, "1");
    assert_eq!(call(&s, "GET", "/w?f=a&v=1", "").1, "1");
    assert_eq!(call(&s, "GET", "/w?f=b&v=y", "").1, "1");
    assert_eq!(call(&s, "GET", "/w?f=b&v=:xy", "").1, "1");
    assert_eq!(call(&s, "GET", "/w?f=b&v=nope", "").1, "0");
}

#[test]
fn field_access_after_db_call() {
    let s = chain_server();
    assert_eq!(call(&s, "GET", "/topname", "").1, "bob");
    assert_eq!(call(&s, "GET", "/one/3", "").1, "cid");
    assert_eq!(call(&s, "GET", "/one/99", "").0, 404);
}

#[test]
fn chain_compile_errors() {
    assert!(compile(r#"GET /a => db.x.create(body).where("a", "b")"#, None).is_err());
    assert!(compile(r#"GET /a => db.x.where("a", "b").delete("1")"#, None).is_err());
    assert!(compile(r#"GET /a => db.x.count().order("a")"#, None).is_err());
    assert!(compile(r#"GET /a => db.x.where("a").count()"#, None).is_err());
    assert!(compile(r#"GET /a => db.x.where("a", "b").nope()"#, None).is_err());
    assert!(compile(r#"GET /a => db.x.where("a", "b").page(0, 5)"#, None).is_ok());
}

#[test]
fn big_lists_keep_their_cache_across_writes() {
    let store = velo::Store::new();
    let s = Server::new(compile(SRC, Some(store.clone())).unwrap()).unwrap();
    let pad = "p".repeat(200);
    for i in 0..3000 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"n{i}","pad":"{pad}"}}"#));
    }
    let listed = call(&s, "GET", "/users", "").1;
    assert!(listed.len() > 512 << 10, "test needs a list past VELO_APPEND_MAX: {}", listed.len());

    for i in 0..10 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"late{i}","pad":"{pad}"}}"#));
        let seen = call(&s, "GET", "/users", "").1;
        assert_eq!(seen.matches(r#""name""#).count(), 3001 + i);
        assert!(seen.contains(&format!(r#""name":"late{i}""#)), "missing the row just written");
    }
    assert_eq!(call(&s, "GET", "/stats", "").1, r#"{"users":3010}"#);
}

#[test]
fn a_reader_holding_the_list_still_sees_later_writes() {
    let store = velo::Store::new();
    let s = Server::new(compile(SRC, Some(store.clone())).unwrap()).unwrap();
    let users = store.collection("users");
    for i in 0..20 {
        call(&s, "POST", "/users", &format!(r#"{{"name":"n{i}"}}"#));
    }
    let held = users.all();
    let before = match &held {
        Value::Raw(bytes) => String::from_utf8(bytes.as_ref().clone()).unwrap(),
        other => panic!("expected rendered bytes, got {other:?}"),
    };
    call(&s, "POST", "/users", r#"{"name":"after"}"#);
    let seen = call(&s, "GET", "/users", "").1;
    assert_eq!(before.matches(r#""name""#).count(), 20);
    assert_eq!(seen.matches(r#""name""#).count(), 21);
    assert!(seen.contains(r#""name":"after""#));
    drop(held);
}

const CMP_SRC: &str = r#"
POST /users   => db.users.create(body)
GET  /over    => db.users.where("score", ">", query.n)
GET  /atleast => db.users.where("score", ">=", query.n).order("name")
GET  /under   => db.users.where("score", "<", query.n).count()
GET  /band    => db.users.where("score", ">=", query.lo).where("score", "<=", query.hi).order("score")
GET  /others  => db.users.where("team", "!=", query.t).order("name")
GET  /after   => db.users.where("name", ">", query.n).order("name")
GET  /oldest  => db.users.where("score", ">", query.n).order("score").first()
GET  /exact   => db.users.first("score", "==", query.n)
"#;

fn cmp_server() -> Arc<Server> {
    let s = Server::new(compile(CMP_SRC, None).unwrap()).unwrap();
    for (name, team, score) in
        [("ann", "red", 5), ("bob", "blue", 9), ("cid", "red", 7), ("dan", "red", 1)]
    {
        let body = format!(r#"{{"name":"{name}","team":"{team}","score":{score}}}"#);
        assert_eq!(call(&s, "POST", "/users", &body).0, 201);
    }
    s
}

#[test]
fn comparison_filters() {
    let s = cmp_server();
    assert_eq!(names(&call(&s, "GET", "/over?n=5", "").1), ["bob", "cid"]);
    assert_eq!(names(&call(&s, "GET", "/atleast?n=5", "").1), ["ann", "bob", "cid"]);
    assert_eq!(call(&s, "GET", "/under?n=5", "").1, "1");
    assert_eq!(names(&call(&s, "GET", "/band?lo=5&hi=7", "").1), ["ann", "cid"]);
    assert_eq!(names(&call(&s, "GET", "/others?t=red", "").1), ["bob"]);
    assert_eq!(names(&call(&s, "GET", "/after?n=bob", "").1), ["cid", "dan"]);
    assert_eq!(
        call(&s, "GET", "/oldest?n=1", "").1,
        r#"{"id":1,"name":"ann","team":"red","score":5}"#
    );
    assert_eq!(
        call(&s, "GET", "/exact?n=9", "").1,
        r#"{"id":2,"name":"bob","team":"blue","score":9}"#
    );
    assert_eq!(call(&s, "GET", "/exact?n=100", "").0, 404);
}

#[test]
fn comparison_filters_ignore_missing_and_unordered_fields() {
    let s = cmp_server();
    assert_eq!(call(&s, "POST", "/users", r#"{"name":"eve","team":"red"}"#).0, 201);
    assert_eq!(call(&s, "POST", "/users", r#"{"name":"fay","team":"red","score":"12"}"#).0, 201);
    assert_eq!(names(&call(&s, "GET", "/over?n=5", "").1), ["bob", "cid", "fay"]);
    assert_eq!(call(&s, "GET", "/under?n=100", "").1, "5");
    assert_eq!(
        names(&call(&s, "GET", "/others?t=blue", "").1),
        ["ann", "cid", "dan", "eve", "fay"]
    );
}

#[test]
fn comparison_needs_a_literal_operator() {
    assert!(compile(r#"GET /a => db.x.where("n", ">", 1)"#, None).is_ok());
    assert!(compile(r#"GET /a => db.x.where("n", "=>", 1)"#, None).is_err());
    assert!(compile(r#"GET /a => db.x.where("n", query.op, 1)"#, None).is_err());
    assert!(compile(r#"GET /a => db.x.where("n", ">", 1).count()"#, None).is_ok());
    assert!(compile(r#"GET /a => db.x.first("n", ">=", 1)"#, None).is_ok());
    assert!(compile(r#"GET /a => db.x.where("n", ">", 1, 2)"#, None).is_err());
}

const LAZY_SRC: &str = r#"
GET /one   => header.x_team
GET /all    => header
GET /qone  => query.a
GET /qall   => query
GET /mixed  => { t: header.x_team, a: query.a, both: header.x_key == query.a }
"#;

#[test]
fn header_fields_are_read_without_building_an_object() {
    let s = Server::new(compile(LAZY_SRC, None).unwrap()).unwrap();
    let raw = b"GET /one HTTP/1.1\r\nHost: x\r\nX-Team: red\r\nX-Key: k\r\nX-Team: blue\r\n\r\n";
    let mut out = Vec::new();
    s.handle("GET", "/one", b"", raw, &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), "red", "first header wins");

    let mut out = Vec::new();
    s.handle("GET", "/one", b"", b"GET /one HTTP/1.1\r\nx-team:  spaced  \r\n\r\n", &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), "spaced");

    let mut out = Vec::new();
    s.handle("GET", "/one", b"", b"GET /one HTTP/1.1\r\nHost: x\r\n\r\n", &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), "null", "a missing header reads as null");

    let mut out = Vec::new();
    s.handle("GET", "/all", b"", raw, &mut out);
    let all = String::from_utf8(out).unwrap();
    assert!(all.contains(r#""x_team":"red""#) && all.contains(r#""x_key":"k""#), "{all}");
}

#[test]
fn query_fields_are_read_without_building_an_object() {
    let s = Server::new(compile(LAZY_SRC, None).unwrap()).unwrap();
    assert_eq!(call(&s, "GET", "/qone?a=1&b=2", "").1, "1");
    assert_eq!(call(&s, "GET", "/qone?b=2&a=hi+there", "").1, "hi there");
    assert_eq!(call(&s, "GET", "/qone?b=2&a%5B%5D=x&a=late", "").1, "late");
    assert_eq!(call(&s, "GET", "/qone?a=", "").1, "");
    assert_eq!(call(&s, "GET", "/qone", "").0, 200);
    assert_eq!(call(&s, "GET", "/qall?a=1&b=2", "").1, r#"{"a":"1","b":"2"}"#);

    let raw = b"GET /mixed?a=k HTTP/1.1\r\nX-Team: red\r\nX-Key: k\r\n\r\n";
    let mut out = Vec::new();
    s.handle("GET", "/mixed?a=k", b"", raw, &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), r#"{"t":"red","a":"k","both":true}"#);
}

const LIB_SRC: &str = r#"
POST /users   => db.users.create({ name: trim(body.name), email: lower(trim(body.email)) })
GET  /users   => db.users.page(default(query.offset, 0), default(query.limit, 20))
GET  /shout   => upper(query.q)
GET  /pick    => { a: default(query.a, "fallback"), n: default(query.n, 7), z: default(query.z, null) }
GET  /folded  => { up: upper("velo"), pad: trim("  x  "), miss: default(null, "yes") }
GET  /count   => db.users.where("email", lower(query.mail)).count()
"#;

#[test]
fn text_and_default_builtins() {
    let s = Server::new(compile(LIB_SRC, None).unwrap()).unwrap();
    assert_eq!(call(&s, "GET", "/shout?q=hi+there", "").1, "HI THERE");
    assert_eq!(call(&s, "GET", "/shout", "").1, "null");
    assert_eq!(
        call(&s, "GET", "/pick", "").1,
        r#"{"a":"fallback","n":7,"z":null}"#,
        "a missing field falls back"
    );
    assert_eq!(
        call(&s, "GET", "/pick?a=&n=0&z=kept", "").1,
        r#"{"a":"fallback","n":"0","z":"kept"}"#,
        "empty falls back, a real 0 does not"
    );
}

#[test]
fn pure_builtins_fold_at_compile_time() {
    let prog = compile(LIB_SRC, None).unwrap();
    let folded = prog.routes.iter().find(|r| r.pattern == "/folded").unwrap();
    assert_eq!(
        folded.konst.as_deref(),
        Some(br#"{"up":"VELO","pad":"x","miss":"yes"}"#.as_slice())
    );
}

#[test]
fn defaults_drive_paging_and_normalised_writes() {
    let s = Server::new(compile(LIB_SRC, None).unwrap()).unwrap();
    for i in 0..25 {
        let body = format!(r#"{{"name":"  n{i}  ","email":"  N{i}@Example.COM "}}"#);
        assert_eq!(call(&s, "POST", "/users", &body).0, 201);
    }
    assert_eq!(names(&call(&s, "GET", "/users", "").1).len(), 20, "default limit applies");
    assert_eq!(names(&call(&s, "GET", "/users?limit=5", "").1).len(), 5);
    assert_eq!(
        names(&call(&s, "GET", "/users?offset=20", "").1),
        ["n20", "n21", "n22", "n23", "n24"]
    );
    assert_eq!(call(&s, "GET", "/count?mail=N3@EXAMPLE.com", "").1, "1");
}

#[test]
fn builtin_arity_is_checked() {
    assert!(compile(r#"GET /a => default(query.x)"#, None).is_err());
    assert!(compile(r#"GET /a => lower("A", "B")"#, None).is_err());
    assert!(compile(r#"GET /a => trim()"#, None).is_err());
    assert!(compile(r#"GET /a => upper(query.x)"#, None).is_ok());
}

const STRESS_SRC: &str = r#"
POST /users     => db.users.create(body)
DELETE /users/:id => db.users.delete(id)
GET  /all       => db.users.all()
GET  /page      => db.users.where("team", query.t).order("score").page(0, 5)
GET  /howmany   => db.users.where("team", query.t).count()
GET  /list      => db.users.where("team", query.t)
GET  /high      => db.users.where("score", ">=", query.n).count()
GET  /best      => db.users.where("team", query.t).order("-score").first()
GET  /total     => db.users.where("team", query.t).sum("score")
GET  /stats     => { users: db.users.count() }
"#;

#[test]
fn chained_reads_stay_consistent_under_writes() {
    let s = Server::new(compile(STRESS_SRC, None).unwrap()).unwrap();
    for i in 0..200 {
        let body = format!(r#"{{"name":"s{i}","team":"t{}","score":{}}}"#, i % 3, i % 50);
        assert_eq!(call(&s, "POST", "/users", &body).0, 201);
    }
    for round in 0..3 {
        chain_stress_round(&s, round);
        verify_chains(&s);
    }
}

fn chain_stress_round(s: &Arc<Server>, round: usize) {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let readers: Vec<_> =
        ["/page?t=t0", "/howmany?t=t1", "/high?n=25", "/best?t=t2", "/total?t=t0"]
            .iter()
            .map(|path| {
                let (s, stop, path) = (s.clone(), stop.clone(), path.to_string());
                std::thread::spawn(move || {
                    let mut seen = 0u64;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let mut out = Vec::new();
                        let (status, _) = s.dispatch("GET", &path, b"", &mut out);
                        assert!(status == 200 || status == 404, "{path} answered {status}");
                        let body = String::from_utf8(out).unwrap();
                        if status == 404 {
                            seen += 1;
                            continue;
                        }
                        let value = parse_json(body.as_bytes())
                            .unwrap_or_else(|e| panic!("{path} sent {body}: {e:?}"));
                        if let Value::Arr(rows) = &value {
                            assert!(rows.len() <= 5, "{path} paged past its limit");
                            let mut last = f64::MIN;
                            for row in rows.iter() {
                                assert_eq!(row.get("team").as_key(), "t0", "{path} leaked a row");
                                let score = match row.get("score") {
                                    Value::Num(n) => n,
                                    other => panic!("{path} row without a score: {other:?}"),
                                };
                                assert!(score >= last, "{path} came back unsorted");
                                last = score;
                            }
                        }
                        seen += 1;
                    }
                    seen
                })
            })
            .collect();

    let writers: Vec<_> = (0..3)
        .map(|w| {
            let s = s.clone();
            std::thread::spawn(move || {
                for i in 0..150 {
                    let mut out = Vec::new();
                    let body = format!(
                        r#"{{"name":"r{round}w{w}x{i}","team":"t{}","score":{}}}"#,
                        i % 3,
                        i % 50
                    );
                    assert_eq!(s.dispatch("POST", "/users", body.as_bytes(), &mut out).0, 201);
                }
            })
        })
        .collect();

    let deleter = {
        let s = s.clone();
        std::thread::spawn(move || {
            for id in (round * 100 + 1)..=(round * 100 + 100) {
                let mut out = Vec::new();
                s.dispatch("DELETE", &format!("/users/{id}"), b"", &mut out);
            }
        })
    };

    for w in writers {
        w.join().unwrap();
    }
    deleter.join().unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let reads: u64 = readers.into_iter().map(|r| r.join().unwrap()).sum();
    assert!(reads > 0, "readers never ran");
}

fn verify_chains(s: &Arc<Server>) {
    let all = match parse_json(call(s, "GET", "/all", "").1.as_bytes()).unwrap() {
        Value::Arr(rows) => rows.as_ref().clone(),
        other => panic!("not a list: {other:?}"),
    };
    assert_eq!(call(s, "GET", "/stats", "").1, format!(r#"{{"users":{}}}"#, all.len()));
    for team in ["t0", "t1", "t2"] {
        let mine: Vec<&Value> = all.iter().filter(|r| r.get("team").as_key() == team).collect();
        let sum: f64 = mine
            .iter()
            .map(|r| match r.get("score") {
                Value::Num(n) => n,
                _ => 0.0,
            })
            .sum();
        assert_eq!(
            call(s, "GET", &format!("/howmany?t={team}"), "").1,
            mine.len().to_string(),
            "count for {team}"
        );
        assert_eq!(
            call(s, "GET", &format!("/total?t={team}"), "").1,
            format!("{sum}"),
            "sum for {team}"
        );
        assert_eq!(
            names(&call(s, "GET", &format!("/list?t={team}"), "").1).len(),
            mine.len(),
            "list for {team}"
        );
    }
    let over = all.iter().filter(|r| matches!(r.get("score"), Value::Num(n) if n >= 25.0)).count();
    assert_eq!(call(s, "GET", "/high?n=25", "").1, over.to_string());
}

const AUTH_SRC: &str = r#"
POST /signup  => db.users.create({ id: body.email, email: body.email, pass: password(body.pass) }) when body.email and body.pass else 400
POST /login   => db.sessions.create({ id: uuid(), user: body.email }) when verify(body.pass, db.users.find(body.email).pass) else 401
GET  /me      => db.sessions.find(header.x_token).user when db.sessions.where("id", header.x_token).count()
GET  /fixed   => hash("velo")
GET  /digest  => hash(query.q)
"#;

fn auth_server() -> Arc<Server> {
    std::env::set_var("VELO_KDF_ROUNDS", "1000");
    Server::new(compile(AUTH_SRC, None).unwrap()).unwrap()
}

fn with_token(s: &Server, token: &str) -> (u16, String) {
    let raw = format!("GET /me HTTP/1.1\r\nhost: x\r\nx-token: {token}\r\n\r\n");
    let mut out = Vec::new();
    let (status, _) = s.handle("GET", "/me", b"", raw.as_bytes(), &mut out);
    (status, String::from_utf8(out).unwrap())
}

#[test]
fn a_password_never_lands_in_the_store_as_written() {
    let s = auth_server();
    let (code, stored, _) = call(&s, "POST", "/signup", r#"{"email":"a@b.c","pass":"hunter2"}"#);
    assert_eq!(code, 201);
    assert!(!stored.contains("hunter2"), "the plain password was stored: {stored}");
    assert!(stored.contains("pbkdf2$1000$"), "{stored}");
    let again = call(&s, "POST", "/signup", r#"{"email":"d@e.f","pass":"hunter2"}"#).1;
    assert_ne!(stored, again, "the same password hashed to the same bytes twice");
}

#[test]
fn login_admits_the_right_password_only() {
    let s = auth_server();
    assert_eq!(call(&s, "POST", "/signup", r#"{"email":"a@b.c","pass":"hunter2"}"#).0, 201);
    assert_eq!(call(&s, "POST", "/signup", r#"{"email":"a@b.c"}"#).0, 400);
    assert_eq!(call(&s, "POST", "/login", r#"{"email":"a@b.c","pass":"nope"}"#).0, 401);
    assert_eq!(call(&s, "POST", "/login", r#"{"email":"gone@b.c","pass":"hunter2"}"#).0, 401);

    let (code, session, _) = call(&s, "POST", "/login", r#"{"email":"a@b.c","pass":"hunter2"}"#);
    assert_eq!(code, 201);
    let token = parse_json(session.as_bytes()).unwrap().get("id").as_key();
    assert_eq!(token.len(), 36);
    assert_eq!(with_token(&s, &token), (200, "a@b.c".to_string()));
    assert_eq!(with_token(&s, "00000000-0000-4000-8000-000000000000").0, 401);
    assert_eq!(with_token(&s, "").0, 401);
}

#[test]
fn hash_folds_when_its_input_is_constant() {
    let prog = compile(AUTH_SRC, None).unwrap();
    let folded = prog.routes.iter().find(|r| r.pattern == "/fixed").unwrap();
    assert_eq!(
        folded.konst.as_deref(),
        Some("cd1e45e8b94d27b2562ab5ae45b4bff61e2bc89d9ca4ffe117e70aa5ac8ef1eb".as_bytes())
    );
    let live = prog.routes.iter().find(|r| r.pattern == "/digest").unwrap();
    assert!(live.konst.is_none());
    let signup = prog.routes.iter().find(|r| r.pattern == "/signup").unwrap();
    assert!(signup.konst.is_none(), "a hashed password must never be folded to a constant");
}

const COOKIE_SRC: &str = r#"
GET /read       => cookie.session
GET /both       => { c: cookie.session, h: header.x_token }
POST /set       => setcookie("session", body.token)
POST /clear     => [setcookie("session", ""), "gone"]
POST /odd       => setcookie("bad name", "x")
POST /injected  => setcookie("session", body.token)
"#;

fn cookie_call(
    s: &Server,
    method: &str,
    path: &str,
    jar: &str,
    body: &str,
) -> (u16, String, String) {
    let raw = format!("{method} {path} HTTP/1.1\r\nhost: x\r\ncookie: {jar}\r\n\r\n");
    let mut out = Vec::new();
    let mut set = Vec::new();
    let (status, _, _, _) =
        s.handle_full(method, path, body.as_bytes(), raw.as_bytes(), &mut out, &mut set);
    (status, String::from_utf8(out).unwrap(), String::from_utf8(set).unwrap())
}

#[test]
fn a_cookie_is_read_by_name() {
    let s = Server::new(compile(COOKIE_SRC, None).unwrap()).unwrap();
    assert_eq!(cookie_call(&s, "GET", "/read", "session=abc", "").1, "abc");
    assert_eq!(cookie_call(&s, "GET", "/read", "a=1; session=abc; b=2", "").1, "abc");
    assert_eq!(cookie_call(&s, "GET", "/read", "a=1;session=abc", "").1, "abc");
    assert_eq!(cookie_call(&s, "GET", "/read", "session=\"abc\"", "").1, "abc");
    assert_eq!(cookie_call(&s, "GET", "/read", "sessionx=abc", "").1, "null");
    assert_eq!(cookie_call(&s, "GET", "/read", "session=", "").1, "");
    assert_eq!(cookie_call(&s, "GET", "/read", "", "").1, "null");
    assert_eq!(cookie_call(&s, "GET", "/read", "novalue", "").1, "null");
    let mut out = Vec::new();
    s.handle("GET", "/read", b"", b"GET /read HTTP/1.1\r\nhost: x\r\n\r\n", &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), "null", "no cookie header at all");
}

#[test]
fn setcookie_writes_a_hardened_header() {
    let s = Server::new(compile(COOKIE_SRC, None).unwrap()).unwrap();
    let (status, body, set) = cookie_call(&s, "POST", "/set", "", r#"{"token":"abc123"}"#);
    assert_eq!((status, body.as_str()), (201, "abc123"), "the value flows through");
    assert_eq!(set, "Set-Cookie: session=abc123; Path=/; HttpOnly; SameSite=Lax\r\n");

    let (_, body, set) = cookie_call(&s, "POST", "/clear", "", "");
    assert!(set.contains("Max-Age=0"), "an empty value expires the cookie: {set}");
    assert!(body.contains("gone"), "the rest of the expression still runs");

    assert_eq!(cookie_call(&s, "POST", "/odd", "", "").2, "", "a bad cookie name sets nothing");
}

#[test]
fn a_cookie_value_cannot_forge_a_header() {
    let s = Server::new(compile(COOKIE_SRC, None).unwrap()).unwrap();
    let body = r#"{"token":"a\r\nX-Admin: yes\r\nb"}"#;
    assert_eq!(cookie_call(&s, "POST", "/injected", "", body).2, "", "crlf must set nothing");
    for bad in [r#"a; Domain=evil.test"#, "a b", "a=b", "a,b", "a\"b"] {
        let body = format!(r#"{{"token":"{bad}"}}"#);
        let set = cookie_call(&s, "POST", "/injected", "", &body).2;
        assert_eq!(set, "", "a value velo cannot write verbatim must set nothing: {bad:?}");
    }
    let set = cookie_call(&s, "POST", "/injected", "", r#"{"token":"ok.value-9_~"}"#).2;
    assert!(set.starts_with("Set-Cookie: session=ok.value-9_~;"), "{set:?}");
}

#[test]
fn cookies_and_headers_are_read_from_the_same_request() {
    let s = Server::new(compile(COOKIE_SRC, None).unwrap()).unwrap();
    let raw = b"GET /both HTTP/1.1\r\nhost: x\r\ncookie: session=c1\r\nx-token: h1\r\n\r\n";
    let mut out = Vec::new();
    s.handle("GET", "/both", b"", raw, &mut out);
    assert_eq!(String::from_utf8(out).unwrap(), r#"{"c":"c1","h":"h1"}"#);
}

const PROJ_SRC: &str = r#"
POST /users      => db.users.create(body)
GET  /u/:id      => db.users.find(id).select("id", "name")
GET  /raw/:id    => db.users.find(id)
GET  /byname     => db.users.first("name", query.n).select("id", "team")
GET  /best       => db.users.order("-score").first().select("name", "score")
GET  /gap/:id    => db.users.find(id).select("id", "nope")
GET  /order/:id  => db.users.find(id).select("team", "id")
GET  /then/:id   => db.users.find(id).select("id", "name").name
"#;

fn proj_server() -> Arc<Server> {
    let s = Server::new(compile(PROJ_SRC, None).unwrap()).unwrap();
    for (name, team, score) in [("ann", "red", 5), ("bob", "blue", 9)] {
        let body =
            format!(r#"{{"name":"{name}","team":"{team}","score":{score},"pass":"SECRET"}}"#);
        assert_eq!(call(&s, "POST", "/users", &body).0, 201);
    }
    s
}

#[test]
fn a_single_row_can_hide_the_fields_it_does_not_want_to_show() {
    let s = proj_server();
    assert!(call(&s, "GET", "/raw/1", "").1.contains("SECRET"), "the unprojected row still leaks");
    for path in ["/u/1", "/byname?n=ann", "/order/1", "/then/1"] {
        assert!(!call(&s, "GET", path, "").1.contains("SECRET"), "{path} leaked the password");
    }
    assert_eq!(call(&s, "GET", "/u/1", "").1, r#"{"id":1,"name":"ann"}"#);
    assert_eq!(call(&s, "GET", "/byname?n=bob", "").1, r#"{"id":2,"team":"blue"}"#);
    assert_eq!(call(&s, "GET", "/best", "").1, r#"{"name":"bob","score":9}"#);
}

#[test]
fn a_projection_keeps_the_order_it_names_and_the_misses_it_does_not_have() {
    let s = proj_server();
    assert_eq!(call(&s, "GET", "/order/1", "").1, r#"{"team":"red","id":1}"#, "named order wins");
    assert_eq!(call(&s, "GET", "/gap/1", "").1, r#"{"id":1}"#, "a field the row lacks is skipped");
    assert_eq!(call(&s, "GET", "/then/1", "").1, "ann", "a field still reads off a projection");
}

#[test]
fn a_projected_miss_is_still_a_miss() {
    let s = proj_server();
    assert_eq!(call(&s, "GET", "/u/99", "").0, 404);
    assert_eq!(call(&s, "GET", "/byname?n=nobody", "").0, 404);
    let empty = Server::new(compile(PROJ_SRC, None).unwrap()).unwrap();
    assert_eq!(call(&empty, "GET", "/best", "").0, 404, "an empty chain still answers 404");
}

#[test]
fn select_still_refuses_where_it_never_belonged() {
    let bad = [
        r#"GET /a => db.x.all().select("id").count()"#,
        r#"GET /a => db.x.count().select("id")"#,
        r#"GET /a => db.x.find("1").select()"#,
        r#"GET /a => db.x.create(body).select("id")"#,
    ];
    for src in bad {
        assert!(compile(src, None).is_err(), "should not compile: {src}");
    }
}

const MASS_SRC: &str = r#"
POST /open  => db.users.create(body)
POST /safe  => db.users.create(body.select("name", "email")) when body.name else 400
POST /echo  => body.select("name")
POST /deep  => body.items.select("id")
POST /none  => body.select("nope")
GET  /flat  => query.q.select("a")
"#;

#[test]
fn a_body_can_be_narrowed_before_it_is_stored() {
    let s = Server::new(compile(MASS_SRC, None).unwrap()).unwrap();
    let body = r#"{"name":"ann","email":"a@b.c","role":"admin","pass":"x"}"#;
    let open = call(&s, "POST", "/open", body).1;
    assert!(
        open.contains("admin") && open.contains(r#""pass""#),
        "unguarded create takes all: {open}"
    );
    assert_eq!(call(&s, "POST", "/safe", body).1, r#"{"id":2,"name":"ann","email":"a@b.c"}"#);
    assert_eq!(call(&s, "POST", "/echo", body).1, r#"{"name":"ann"}"#);
    assert_eq!(call(&s, "POST", "/safe", r#"{"role":"admin"}"#).0, 400, "a guard still gates it");
}

#[test]
fn select_maps_over_an_array_and_gives_up_on_anything_else() {
    let s = Server::new(compile(MASS_SRC, None).unwrap()).unwrap();
    assert_eq!(
        call(&s, "POST", "/deep", r#"{"items":[{"id":1,"s":"x"},{"id":2,"s":"y"}]}"#).1,
        r#"[{"id":1},{"id":2}]"#
    );
    assert_eq!(call(&s, "POST", "/none", r#"{"a":1}"#).1, "{}", "no field matched");
    assert_eq!(call(&s, "GET", "/flat?q=hello", "").1, "null", "a string has no fields");
    assert_eq!(call(&s, "POST", "/echo", "").1, "null", "no body at all");
    assert_eq!(call(&s, "POST", "/deep", r#"{"items":[[1],"x",null]}"#).1, "[null,null,null]");
}

#[test]
fn a_bare_select_is_refused_wherever_it_appears() {
    for src in [
        r#"POST /a => body.select()"#,
        r#"POST /a => db.x.create(body.select())"#,
        r#"GET /a => query.q.select()"#,
    ] {
        assert!(compile(src, None).is_err(), "should not compile: {src}");
    }
}

const GC_SRC: &str = r#"
POST /add    => db.jobs.create(body)
GET  /count  => db.jobs.count()
DELETE /old  => db.jobs.delete_where("score", "<", query.n)
DELETE /team => db.jobs.delete_where("team", query.t)
DELETE /not  => db.jobs.delete_where("team", "!=", query.t)
"#;

fn gc_server() -> Arc<Server> {
    let s = Server::new(compile(GC_SRC, None).unwrap()).unwrap();
    for (team, score) in [("red", 1), ("red", 5), ("blue", 9), ("blue", 20)] {
        let body = format!(r#"{{"team":"{team}","score":{score}}}"#);
        assert_eq!(call(&s, "POST", "/add", &body).0, 201);
    }
    s
}

#[test]
fn delete_where_takes_the_same_operators_as_where() {
    let s = gc_server();
    assert_eq!(call(&s, "DELETE", "/old?n=6", "").1, r#"{"deleted":2}"#);
    assert_eq!(call(&s, "GET", "/count", "").1, "2");
    assert_eq!(call(&s, "DELETE", "/old?n=6", "").1, r#"{"deleted":0}"#);

    let s = gc_server();
    assert_eq!(
        call(&s, "DELETE", "/team?t=red", "").1,
        r#"{"deleted":2}"#,
        "no operator is still =="
    );
    assert_eq!(call(&s, "GET", "/count", "").1, "2");

    let s = gc_server();
    assert_eq!(call(&s, "DELETE", "/not?t=red", "").1, r#"{"deleted":2}"#);
    assert_eq!(call(&s, "GET", "/count", "").1, "2");
}

#[test]
fn delete_where_wants_a_literal_operator() {
    assert!(compile(r#"DELETE /a => db.x.delete_where("n", ">", 1)"#, None).is_ok());
    assert!(compile(r#"DELETE /a => db.x.delete_where("n", "=>", 1)"#, None).is_err());
    assert!(compile(r#"DELETE /a => db.x.delete_where("n", query.op, 1)"#, None).is_err());
    assert!(compile(r#"DELETE /a => db.x.delete_where("n", ">", 1, 2)"#, None).is_err());
}

const LIMIT_SRC: &str = r#"
POST /login  => "in" when limit("t1:" + body.user, 3) and body.pass == "ok" else 401
GET  /solo   => "ok" when limit("t2:" + query.k, 2) else 401
GET  /zero   => "ok" when limit("t3", 0) else 401
GET  /open   => "open"
"#;

#[test]
fn a_limit_counts_per_key_and_answers_429_whatever_the_guard_says() {
    let s = Server::new(compile(LIMIT_SRC, None).unwrap()).unwrap();
    let login = |user: &str, pass: &str| {
        call(&s, "POST", "/login", &format!(r#"{{"user":"{user}","pass":"{pass}"}}"#)).0
    };
    assert_eq!([login("ann", "ok"), login("ann", "ok"), login("ann", "ok")], [201, 201, 201]);
    assert_eq!(login("ann", "ok"), 429, "over the ceiling, not the guard's 401");
    assert_eq!(login("bob", "ok"), 201, "a different key has its own budget");
    assert_eq!(login("cid", "WRONG"), 401, "a wrong password is still 401");
    assert_eq!([login("cid", "WRONG"), login("cid", "WRONG")], [401, 401]);
    assert_eq!(login("cid", "WRONG"), 429, "failed attempts count toward the ceiling");
    assert_eq!(call(&s, "GET", "/open", "").0, 200, "an unlimited route is untouched");
}

#[test]
fn a_limit_of_zero_lets_nothing_through() {
    let s = Server::new(compile(LIMIT_SRC, None).unwrap()).unwrap();
    assert_eq!(call(&s, "GET", "/zero", "").0, 429);
    assert_eq!(call(&s, "GET", "/solo?k=x", "").0, 200);
    assert_eq!(call(&s, "GET", "/solo?k=x", "").0, 200);
    assert_eq!(call(&s, "GET", "/solo?k=x", "").0, 429);
    assert_eq!(call(&s, "GET", "/solo?k=y", "").0, 200);
    assert_eq!(call(&s, "GET", "/solo?k=x", "").1, r#"{"error":"too many requests"}"#);
}

#[test]
fn limit_wants_a_key_and_a_rate() {
    assert!(compile(r#"GET /a => "x" when limit("k", 5)"#, None).is_ok());
    assert!(compile(r#"GET /a => "x" when limit("k")"#, None).is_err());
    assert!(compile(r#"GET /a => "x" when limit("k", 5, 6)"#, None).is_err());
    let prog = compile(r#"GET /a => "x" when limit("k", 5)"#, None).unwrap();
    assert!(prog.routes[0].konst.is_none(), "a limited route must never fold to a constant");
}

const MULTI_SRC: &str = r#"
POST /add    => db.users.create(body)
GET  /both   => db.users.where("team", query.t).where("keep", query.k).count()
GET  /rows   => db.users.where("team", query.t).where("keep", query.k).order("id").select("id")
GET  /paged  => db.users.where("team", query.t).page(0, 5).where("keep", query.k).count()
GET  /three  => db.users.where("team", query.t).where("keep", query.k).where("band", query.b).count()
GET  /mixed  => db.users.where("team", query.t).where("score", ">", query.s).where("keep", query.k).count()
"#;

#[test]
fn several_equality_filters_answer_what_one_scan_would() {
    let s = Server::new(compile(MULTI_SRC, None).unwrap()).unwrap();
    let mut want = std::collections::HashMap::new();
    for i in 0..1500 {
        let (team, keep, band) = (format!("t{}", i % 3), i % 2 == 0, format!("b{}", i % 7));
        let body =
            format!(r#"{{"team":"{team}","keep":{keep},"band":"{band}","score":{}}}"#, i % 100);
        assert_eq!(call(&s, "POST", "/add", &body).0, 201);
        *want.entry((team, keep)).or_insert(0) += 1;
    }
    for team in ["t0", "t1", "t2"] {
        for keep in ["true", "false"] {
            let got: i64 =
                call(&s, "GET", &format!("/both?t={team}&k={keep}"), "").1.parse().unwrap();
            let expect = *want.get(&(team.to_string(), keep == "true")).unwrap_or(&0);
            assert_eq!(got, expect, "{team} keep={keep}");
        }
    }
    assert_eq!(call(&s, "GET", "/both?t=nope&k=true", "").1, "0");
    assert_eq!(call(&s, "GET", "/three?t=t0&k=true&b=b0", "").1, "36");
    let scanned: i64 = call(&s, "GET", "/mixed?t=t0&s=50&k=true", "").1.parse().unwrap();
    assert!(scanned > 0 && scanned < 250, "a non-equality step still narrows: {scanned}");
}

#[test]
fn a_page_before_a_filter_is_not_reordered_by_the_index() {
    let s = Server::new(compile(MULTI_SRC, None).unwrap()).unwrap();
    for i in 0..600 {
        let keep = i >= 5;
        let body = format!(r#"{{"team":"t0","keep":{keep},"band":"b0","score":1}}"#);
        assert_eq!(call(&s, "POST", "/add", &body).0, 201);
    }
    assert_eq!(
        call(&s, "GET", "/paged?t=t0&k=true", "").1,
        "0",
        "the first five rows all have keep=false, so paging first must find none"
    );
    assert_eq!(call(&s, "GET", "/paged?t=t0&k=false", "").1, "5");
    assert_eq!(call(&s, "GET", "/both?t=t0&k=false", "").1, "5");
}

#[test]
fn intersected_filters_follow_later_writes() {
    let s = Server::new(compile(MULTI_SRC, None).unwrap()).unwrap();
    for i in 0..700 {
        let body = format!(r#"{{"team":"t{}","keep":true,"band":"b0","score":1}}"#, i % 2);
        assert_eq!(call(&s, "POST", "/add", &body).0, 201);
    }
    let before: i64 = call(&s, "GET", "/both?t=t0&k=true", "").1.parse().unwrap();
    assert_eq!(before, 350);
    assert_eq!(call(&s, "POST", "/add", r#"{"team":"t0","keep":true,"band":"b0"}"#).0, 201);
    assert_eq!(call(&s, "GET", "/both?t=t0&k=true", "").1, "351", "an insert extends the index");
    let rows = call(&s, "GET", "/rows?t=t0&k=true", "").1;
    assert_eq!(rows.matches(r#""id""#).count(), 351);
}

const TOPN_SRC: &str = r#"
POST /add   => db.users.create(body)
GET  /top   => db.users.where("team", query.t).order("-score").page(0, query.n).select("id")
GET  /all   => db.users.where("team", query.t).order("-score").select("id")
GET  /skip  => db.users.where("team", query.t).order("-score").page(query.o, query.n).select("id")
GET  /open  => db.users.where("team", query.t).order("-score").page(0, 0).select("id")
GET  /after => db.users.order("-score").page(0, 10).where("team", query.t).select("id")
"#;

#[test]
fn a_top_n_matches_sorting_everything_and_taking_n() {
    let s = Server::new(compile(TOPN_SRC, None).unwrap()).unwrap();
    for i in 0..1200 {
        let body = format!(r#"{{"team":"t{}","score":{}}}"#, i % 2, i % 17);
        assert_eq!(call(&s, "POST", "/add", &body).0, 201);
    }
    let full: Vec<String> = call(&s, "GET", "/all?t=t0", "")
        .1
        .split("},{")
        .map(|p| p.trim_matches(|c| c == '[' || c == ']' || c == '{' || c == '}').to_string())
        .collect();
    assert_eq!(full.len(), 600);
    for n in [1usize, 5, 20, 599, 600, 601] {
        let got = call(&s, "GET", &format!("/top?t=t0&n={n}"), "").1;
        let want = format!("[{{{}}}]", full[..n.min(600)].join("},{"));
        assert_eq!(got, want, "top {n} must equal the full sort truncated, ties and all");
    }
    for (o, n) in [(0usize, 10usize), (5, 10), (100, 25), (595, 10)] {
        let got = call(&s, "GET", &format!("/skip?t=t0&o={o}&n={n}"), "").1;
        let slice = &full[o.min(600)..(o + n).min(600)];
        let want = if slice.is_empty() {
            "[]".to_string()
        } else {
            format!("[{{{}}}]", slice.join("},{"))
        };
        assert_eq!(got, want, "offset {o} limit {n}");
    }
    assert_eq!(call(&s, "GET", "/open?t=t0", "").1, call(&s, "GET", "/all?t=t0", "").1);
}

#[test]
fn a_filter_after_a_page_still_sees_the_page() {
    let s = Server::new(compile(TOPN_SRC, None).unwrap()).unwrap();
    for i in 0..800 {
        let team = if i < 10 { "t1" } else { "t0" };
        assert_eq!(call(&s, "POST", "/add", &format!(r#"{{"team":"{team}","score":{i}}}"#)).0, 201);
    }
    assert_eq!(
        call(&s, "GET", "/after?t=t1", "").1,
        "[]",
        "the ten highest scores are all t0, so filtering after paging finds no t1"
    );
    assert_eq!(call(&s, "GET", "/after?t=t0", "").1.matches(r#""id""#).count(), 10);
}

const MIXED_SORT_SRC: &str = r#"
POST /add    => db.mixed.create(body)
GET  /up     => db.mixed.order("v").select("id")
GET  /down   => db.mixed.order("-v").select("id")
GET  /uptop  => db.mixed.order("v").page(0, 4).select("id")
GET  /best   => db.mixed.order("-v").first().select("id")
"#;

#[test]
fn sorting_orders_mixed_types_the_same_way_every_route_does() {
    let s = Server::new(compile(MIXED_SORT_SRC, None).unwrap()).unwrap();
    for body in [
        r#"{"id":"num2","v":2}"#,
        r#"{"id":"text_b","v":"b"}"#,
        r#"{"id":"num10","v":10}"#,
        r#"{"id":"missing"}"#,
        r#"{"id":"null","v":null}"#,
        r#"{"id":"text_a","v":"a"}"#,
        r#"{"id":"bool","v":true}"#,
        r#"{"id":"num2b","v":2}"#,
    ] {
        assert_eq!(call(&s, "POST", "/add", body).0, 201);
    }
    let up = call(&s, "GET", "/up", "").1;
    assert_eq!(
        up,
        r#"[{"id":"num2"},{"id":"num2b"},{"id":"num10"},{"id":"missing"},{"id":"null"},{"id":"text_a"},{"id":"text_b"},{"id":"bool"}]"#,
        "numbers first in value order, then empties, then text, ties in insertion order"
    );
    let down = call(&s, "GET", "/down", "").1;
    assert_eq!(down.matches(r#""id""#).count(), 8);
    assert!(down.starts_with(r#"[{"id":"bool"}"#), "descending flips the whole order: {down}");

    let top: Vec<&str> = up.split("},{").take(4).collect();
    assert_eq!(
        call(&s, "GET", "/uptop", "").1,
        format!("{}}}]", top.join("},{")),
        "a partial sort must agree with the full one on mixed types too"
    );
    assert_eq!(call(&s, "GET", "/best", "").1, r#"{"id":"bool"}"#, "first() agrees as well");
}

const SHAPE_SRC: &str = r#"
POST /add    => db.shapes.create(body)
GET  /pick   => db.shapes.where("k", query.v).select("id")
GET  /sorted => db.shapes.order("k").select("id")
GET  /top    => db.shapes.order("-k").page(0, 3).select("id")
GET  /hunt   => db.shapes.search("k", query.v).select("id")
GET  /gone   => db.shapes.where("k", "<", query.v).count()
GET  /one    => db.shapes.first("k", query.v).select("id")
"#;

#[test]
fn rows_of_different_shapes_are_read_by_name_not_by_position() {
    let s = Server::new(compile(SHAPE_SRC, None).unwrap()).unwrap();
    let bodies = [
        r#"{"id":"a","k":"m","pad":1}"#,
        r#"{"id":"b","pad":1,"k":"m"}"#,
        r#"{"id":"c","x":1,"y":2,"k":"z"}"#,
        r#"{"id":"d","pad":9}"#,
        r#"{"id":"e","k":"m"}"#,
        r#"{"id":"f","p":1,"q":2,"r":3,"s":4,"k":"a"}"#,
    ];
    for body in bodies {
        assert_eq!(call(&s, "POST", "/add", body).0, 201);
    }
    assert_eq!(
        call(&s, "GET", "/pick?v=m", "").1,
        r#"[{"id":"a"},{"id":"b"},{"id":"e"}]"#,
        "the field sits at a different index in each of these rows"
    );
    assert_eq!(call(&s, "GET", "/pick?v=z", "").1, r#"[{"id":"c"}]"#);
    assert_eq!(call(&s, "GET", "/one?v=m", "").1, r#"{"id":"a"}"#);
    assert_eq!(
        call(&s, "GET", "/sorted", "").1,
        r#"[{"id":"d"},{"id":"f"},{"id":"a"},{"id":"b"},{"id":"e"},{"id":"c"}]"#,
        "the row missing the field sorts as empty, then a, then the three m, then z"
    );
    assert_eq!(call(&s, "GET", "/top", "").1, r#"[{"id":"c"},{"id":"a"},{"id":"b"}]"#);
    assert_eq!(call(&s, "GET", "/hunt?v=m", "").1, r#"[{"id":"a"},{"id":"b"},{"id":"e"}]"#);
    assert_eq!(call(&s, "GET", "/gone?v=m", "").1, "1", "only k=a is below m");
}

#[test]
fn a_repeated_key_keeps_the_last_value_once() {
    let s = Server::new(compile(SHAPE_SRC, None).unwrap()).unwrap();
    let (code, body, _) = call(&s, "POST", "/add", r#"{"id":"dup","k":"first","k":"last"}"#);
    assert_eq!(code, 201);
    assert_eq!(body, r#"{"id":"dup","k":"last"}"#, "one k, the later value");
    assert_eq!(body.matches(r#""k""#).count(), 1);
    assert_eq!(call(&s, "GET", "/pick?v=last", "").1, r#"[{"id":"dup"}]"#);
    assert_eq!(call(&s, "GET", "/pick?v=first", "").1, "[]");
}

const PURGE_SRC: &str = r#"
POST /add    => db.jobs.create(body)
GET  /n      => db.jobs.count()
GET  /done   => db.jobs.where("state", "done").count()
GET  /rows   => db.jobs.where("state", "done").select("id")
DELETE /purge => db.jobs.delete_where("state", query.s)
"#;

#[test]
fn a_read_repeated_after_delete_where_sees_the_deletion() {
    let s = Server::new(compile(PURGE_SRC, None).unwrap()).unwrap();
    for i in 0..900 {
        let state = if i % 3 == 0 { "done" } else { "open" };
        let body = format!(r#"{{"id":{i},"state":"{state}"}}"#);
        assert_eq!(call(&s, "POST", "/add", &body).0, 201);
    }
    assert_eq!(call(&s, "GET", "/done", "").1, "300");
    assert_eq!(call(&s, "GET", "/done", "").1, "300", "the second read is served from cache");
    assert_eq!(call(&s, "GET", "/rows", "").1.matches(r#""id""#).count(), 300);

    assert_eq!(call(&s, "DELETE", "/purge?s=done", "").1, r#"{"deleted":300}"#);
    assert_eq!(call(&s, "GET", "/n", "").1, "600");
    assert_eq!(call(&s, "GET", "/done", "").1, "0", "the cached count must not survive the purge");
    assert_eq!(call(&s, "GET", "/rows", "").1, "[]");

    for i in 900..960 {
        assert_eq!(call(&s, "POST", "/add", &format!(r#"{{"id":{i},"state":"done"}}"#)).0, 201);
    }
    assert_eq!(call(&s, "GET", "/done", "").1, "60");
    assert_eq!(call(&s, "DELETE", "/purge?s=open", "").1, r#"{"deleted":600}"#);
    assert_eq!(call(&s, "GET", "/done", "").1, "60", "purging others leaves these alone");
    assert_eq!(call(&s, "GET", "/n", "").1, "60");
}

#[test]
fn a_request_ending_in_bare_newlines_is_still_served() {
    let port = spawn();
    let res = raw(port, b"GET /health HTTP/1.1\nHost: x\nConnection: close\n\n");
    assert!(res.starts_with("HTTP/1.1 200"), "bare LF headers: {res}");
    assert!(res.ends_with("ok"), "{res}");

    let res = raw(port, b"POST /users HTTP/1.1\nHost: x\nContent-Length: 16\nConnection: close\n\n{\"name\":\"lf\"}   ");
    assert!(res.starts_with("HTTP/1.1 201"), "bare LF with a body: {res}");
    assert!(res.contains(r#""name":"lf""#), "{res}");

    let res = raw(port, b"GET /health HTTP/1.1\r\nHost: x\nX-Mixed: y\r\nConnection: close\n\n");
    assert!(res.starts_with("HTTP/1.1 200"), "mixed line endings: {res}");
}

#[test]
fn ids_do_not_restart_after_a_reload() {
    let dir = std::env::temp_dir().join(format!("velo-ids-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("store.json");
    let store = velo::Store::new();
    let s = Server::new(compile(SRC, Some(store.clone())).unwrap()).unwrap();
    for name in ["a", "b", "c", "d", "e"] {
        assert_eq!(call(&s, "POST", "/users", &format!(r#"{{"name":"{name}"}}"#)).0, 201);
    }
    for id in [4, 5] {
        assert_eq!(call(&s, "DELETE", &format!("/users/{id}"), "").0, 200);
    }
    store.save_to(&path).unwrap();

    let back = velo::Store::new();
    back.load_file(&path).unwrap();
    let s2 = Server::new(compile(SRC, Some(back.clone())).unwrap()).unwrap();
    let (code, body, _) = call(&s2, "POST", "/users", r#"{"name":"d"}"#);
    assert_eq!(code, 201, "{body}");
    assert_eq!(
        body, r#"{"id":6,"name":"d"}"#,
        "an id that was handed out before the restart must never be handed out again"
    );
    assert_eq!(names(&call(&s2, "GET", "/users", "").1).len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_snapshot_is_never_half_written() {
    let dir = std::env::temp_dir().join(format!("velo-atomic-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("store.json");
    let store = velo::Store::new();
    let users = store.collection("users");
    for i in 0..4000 {
        let raw = format!(
            r#"{{"id":{i},"name":"user number {i}","note":"padding to make the file large"}}"#
        );
        users.create(parse_json(raw.as_bytes()).unwrap(), &[]).unwrap();
    }
    store.save_to(&path).unwrap();
    let full = std::fs::metadata(&path).unwrap().len();
    assert!(full > 200_000, "the snapshot must be big enough to catch a partial write: {full}");

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let writer = {
        let (store, path, stop, writes) =
            (store.clone(), path.clone(), stop.clone(), writes.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                store.save_to(&path).unwrap();
                writes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        })
    };
    for _ in 0..200 {
        let raw = std::fs::read(&path).unwrap();
        assert!(
            matches!(parse_json(&raw), Ok(velo::Value::Obj(_))),
            "a reader saw {} bytes of a snapshot that was still being written",
            raw.len()
        );
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
    assert!(
        writes.load(std::sync::atomic::Ordering::Relaxed) > 5,
        "the writer barely ran, so the reads raced nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const SEG_SRC: &str = r#"
GET /users/:id     => { got: id }
GET /a/:x/b/:y     => { x: x, y: y }
GET /deep/:a/:b/:c => { a: a, b: b, c: c }
"#;

#[test]
fn an_empty_path_segment_never_fills_a_param() {
    let s = Server::new(compile(SEG_SRC, None).unwrap()).unwrap();
    assert_eq!(call(&s, "GET", "/users/1", "").1, r#"{"got":"1"}"#);
    assert_eq!(call(&s, "GET", "/a/1/b/2", "").1, r#"{"x":"1","y":"2"}"#);
    for path in ["/users/", "/users//", "/a//b/2", "/a/1/b/", "/a//b/", "/deep/1//3", "/deep///"] {
        let (code, body, _) = call(&s, "GET", path, "");
        assert_eq!(code, 404, "{path} matched with an empty param: {body}");
    }
}

const GUARDED_CONST_SRC: &str = r#"
GET /plain  => "open"
GET /gated  => "secret" when header.x_key == "root" else 403
GET /envd   => "secret" when env("VELO_TEST_GATE") == "open" else 403
GET /always => "yes" when 1 == 1
"#;

#[test]
fn a_guarded_route_is_never_folded_and_its_guard_runs_first() {
    let prog = compile(GUARDED_CONST_SRC, None).unwrap();
    let of = |p: &str| prog.routes.iter().find(|r| r.pattern == p).unwrap();
    assert!(of("/plain").konst.is_some(), "an unguarded constant still folds");
    for p in ["/gated", "/envd", "/always"] {
        assert!(of(p).konst.is_none(), "{p} folded to a constant despite its guard");
        assert!(of(p).const_etag.is_none(), "{p} carries a compile-time ETag");
    }

    let s = Server::new(compile(GUARDED_CONST_SRC, None).unwrap()).unwrap();
    let with_key = |key: &str| {
        let req = format!("GET /gated HTTP/1.1\r\nhost: x\r\nx-key: {key}\r\n\r\n");
        let mut out = Vec::new();
        let (code, _) = s.handle("GET", "/gated", b"", req.as_bytes(), &mut out);
        (code, String::from_utf8(out).unwrap())
    };
    assert_eq!(with_key("root"), (200, "secret".to_string()));
    assert_eq!(with_key("nope").0, 403, "the guard must decide before the body is served");
    assert_eq!(call(&s, "GET", "/gated", "").0, 403, "and with no header at all");
    assert!(!call(&s, "GET", "/gated", "").1.contains("secret"), "a refusal must not leak it");
}

const WHY_SRC: &str = r#"
POST /users => db.users.create(body) when body.name else 400 "name is required"
GET  /admin => "ok" when header.x_key == "root" else 403 "admin token required"
GET  /plain => "ok" when header.x_key
GET  /quiet => "ok" when header.x_key else 403
POST /aged  => "ok" when body.age > 0 and body.age < 130 else 400 "age must be between 1 and 129"
"#;

#[test]
fn a_guard_can_say_why_it_refused() {
    let s = Server::new(compile(WHY_SRC, None).unwrap()).unwrap();
    assert_eq!(
        call(&s, "POST", "/users", "{}"),
        (400, r#"{"error":"name is required"}"#.into(), JSON)
    );
    assert_eq!(call(&s, "POST", "/users", r#"{"name":"ann"}"#).0, 201, "a passing guard is quiet");
    assert_eq!(
        call(&s, "POST", "/aged", r#"{"age":200}"#).1,
        r#"{"error":"age must be between 1 and 129"}"#
    );
    let (code, body, _) = call(&s, "GET", "/admin", "");
    assert_eq!((code, body.as_str()), (403, r#"{"error":"admin token required"}"#));
    assert_eq!(call(&s, "GET", "/plain", "").1, r#"{"error":"unauthorized"}"#, "no reason given");
    assert_eq!(
        call(&s, "GET", "/quiet", "").1,
        r#"{"error":"unauthorized"}"#,
        "status but no text"
    );
    assert_eq!(call(&s, "GET", "/quiet", "").0, 403);
}

#[test]
fn a_reason_is_escaped_and_must_not_be_empty() {
    let s = Server::new(
        compile(r#"POST /a => "x" when body.k else 400 "quote \" and \\ and newline""#, None)
            .unwrap(),
    )
    .unwrap();
    let body = call(&s, "POST", "/a", "{}").1;
    assert_eq!(body, r#"{"error":"quote \" and \\ and newline"}"#);
    assert!(matches!(parse_json(body.as_bytes()), Ok(Value::Obj(_))), "still valid JSON: {body}");

    assert!(compile(r#"POST /a => "x" when body.k else 400 """#, None).is_err(), "empty reason");
    assert!(
        compile(r#"POST /a => "x" when body.k else "oops""#, None).is_err(),
        "reason no status"
    );
    assert!(compile(r#"POST /a => "x" else 400 "no guard""#, None).is_err(), "no guard at all");
}

#[test]
fn a_reason_reaches_the_openapi_document() {
    let prog = compile(WHY_SRC, None).unwrap();
    let doc = String::from_utf8(velo::openapi::document(&prog, "t", "1")).unwrap();
    assert!(doc.contains(r#""400":{"description":"name is required""#), "{doc}");
    assert!(doc.contains(r#""403":{"description":"admin token required""#), "{doc}");
    assert!(doc.contains(r#""401":{"description":"error""#), "an unexplained guard stays generic");
    assert!(matches!(parse_json(doc.as_bytes()), Ok(Value::Obj(_))));
}

const CHECK_SRC: &str = r#"
POST /users => db.users.create(body.select("name","email","age")) when check(body.name, "name is required") and check(body.email, "email is required") and check(body.age > 0, "age must be positive") and check(len(body.name) < 20, "name must be under 20 characters")
GET  /mixed => "ok" when header.x_key and check(query.n, "n is required")
GET  /body  => check(query.a, "a is required")
GET  /after => "ok" when check(query.a, "a is required") and header.x_key
"#;

#[test]
fn check_names_the_condition_that_failed() {
    let s = Server::new(compile(CHECK_SRC, None).unwrap()).unwrap();
    let post = |body: &str| {
        let (code, out, _) = call(&s, "POST", "/users", body);
        (code, out)
    };
    assert_eq!(post("{}"), (400, r#"{"error":"name is required"}"#.into()));
    assert_eq!(post(r#"{"name":"ann"}"#), (400, r#"{"error":"email is required"}"#.into()));
    assert_eq!(
        post(r#"{"name":"ann","email":"a@b.c"}"#),
        (400, r#"{"error":"age must be positive"}"#.into())
    );
    assert_eq!(
        post(r#"{"name":"ann","email":"a@b.c","age":-1}"#),
        (400, r#"{"error":"age must be positive"}"#.into())
    );
    assert_eq!(
        post(r#"{"name":"a-name-that-is-far-too-long","email":"a@b.c","age":5}"#),
        (400, r#"{"error":"name must be under 20 characters"}"#.into())
    );
    let (code, body) = post(r#"{"name":"ann","email":"a@b.c","age":5}"#);
    assert_eq!(code, 201);
    assert_eq!(body, r#"{"id":1,"name":"ann","email":"a@b.c","age":5}"#);
}

#[test]
fn check_sits_beside_the_guards_that_were_already_there() {
    let s = Server::new(compile(CHECK_SRC, None).unwrap()).unwrap();
    let with_key = |path: &str, key: Option<&str>| {
        let head = match key {
            Some(k) => format!("GET {path} HTTP/1.1\r\nhost: x\r\nx-key: {k}\r\n\r\n"),
            None => format!("GET {path} HTTP/1.1\r\nhost: x\r\n\r\n"),
        };
        let mut out = Vec::new();
        let (code, _) = s.handle("GET", path, b"", head.as_bytes(), &mut out);
        (code, String::from_utf8(out).unwrap())
    };
    assert_eq!(with_key("/mixed", None), (401, r#"{"error":"unauthorized"}"#.into()));
    assert_eq!(with_key("/mixed", Some("k")), (400, r#"{"error":"n is required"}"#.into()));
    assert_eq!(with_key("/mixed?n=1", Some("k")), (200, "ok".into()));
    assert_eq!(with_key("/after", Some("k")).0, 400, "a failed check wins over the rest");
    assert_eq!(with_key("/after?a=1", None), (401, r#"{"error":"unauthorized"}"#.into()));
    assert_eq!(with_key("/after?a=1", Some("k")), (200, "ok".into()));
}

#[test]
fn a_check_outside_a_guard_answers_the_same_way() {
    let s = Server::new(compile(CHECK_SRC, None).unwrap()).unwrap();
    assert_eq!(call(&s, "GET", "/body", ""), (400, r#"{"error":"a is required"}"#.into(), JSON));
    assert_eq!(call(&s, "GET", "/body?a=1", "").0, 200);
    assert!(compile(r#"GET /a => "x" when check(query.a)"#, None).is_err(), "check needs a reason");
    assert!(compile(r#"GET /a => "x" when check()"#, None).is_err());
    let prog = compile(r#"GET /a => "x" when check(1, "never")"#, None).unwrap();
    assert!(prog.routes[0].konst.is_none(), "a checked route must not fold");
}

const CONCUR_SRC: &str = r#"
GET /why    => "ok" when check(query.a, "a is required")
GET /named  => "ok" when check(query.a, query.r)
GET /cook   => setcookie("s", query.v)
GET /rate   => "ok" when limit(query.k, 5)
GET /free   => "free"
"#;

fn serve_on(src: &str) -> (Arc<Server>, u16) {
    let s = Server::new(compile(src, None).unwrap()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bg = s.clone();
    std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));
    (s, port)
}

#[test]
fn pipelined_requests_never_borrow_each_others_reasons() {
    let (s, port) = serve_on(CONCUR_SRC);
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    let mut batch = String::new();
    let mut want = Vec::new();
    for i in 0..60 {
        match i % 4 {
            0 => {
                batch.push_str(&format!("GET /named?r=reason-{i} HTTP/1.1\r\nHost: x\r\n\r\n"));
                want.push(format!("{{\"error\":\"reason-{i}\"}}"));
            }
            1 => {
                batch.push_str("GET /named?a=1&r=unused HTTP/1.1\r\nHost: x\r\n\r\n");
                want.push("ok".to_string());
            }
            2 => {
                batch.push_str(&format!("GET /cook?v=tok-{i} HTTP/1.1\r\nHost: x\r\n\r\n"));
                want.push(format!("tok-{i}"));
            }
            _ => {
                batch.push_str("GET /free HTTP/1.1\r\nHost: x\r\n\r\n");
                want.push("free".to_string());
            }
        }
    }
    batch.push_str("GET /free HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    want.push("free".to_string());
    c.write_all(batch.as_bytes()).unwrap();

    let mut out = Vec::new();
    c.read_to_end(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let mut bodies = Vec::new();
    let mut rest = text.as_str();
    while let Some(gap) = rest.find("\r\n\r\n") {
        let (head, after) = rest.split_at(gap + 4);
        let len: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        bodies.push(after[..len].to_string());
        rest = &after[len..];
    }
    assert_eq!(bodies.len(), want.len(), "one body per request: {}", bodies.len());
    for (got, expect) in bodies.iter().zip(&want) {
        assert_eq!(got, expect, "a pipelined response carried the wrong body");
    }
    assert_eq!(text.matches("Set-Cookie: s=tok-").count(), 15, "one cookie per cookie request");
    for i in (2..60).step_by(4) {
        assert!(text.contains(&format!("Set-Cookie: s=tok-{i};")), "missing cookie {i}");
    }
    s.shutdown();
}

#[test]
fn a_rate_ceiling_holds_when_many_threads_push_at_once() {
    let s = Server::new(compile(CONCUR_SRC, None).unwrap()).unwrap();
    let key = format!("burst-{}", std::process::id());
    let passed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refused = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut hands = Vec::new();
    for _ in 0..8 {
        let (s, key, passed, refused) = (s.clone(), key.clone(), passed.clone(), refused.clone());
        hands.push(std::thread::spawn(move || {
            for _ in 0..40 {
                let mut out = Vec::new();
                let (code, _) = s.dispatch("GET", &format!("/rate?k={key}"), b"", &mut out);
                match code {
                    200 => passed.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    429 => refused.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    other => panic!("unexpected {other}"),
                };
            }
        }));
    }
    for h in hands {
        h.join().unwrap();
    }
    let (ok, no) = (
        passed.load(std::sync::atomic::Ordering::Relaxed),
        refused.load(std::sync::atomic::Ordering::Relaxed),
    );
    assert_eq!(ok + no, 320, "every request got an answer");
    assert!(ok >= 5, "the ceiling let its five through: {ok}");
    assert!(ok <= 5 * 3, "320 requests in a moment must not pass a ceiling of five: {ok}");
}

#[test]
fn threads_hitting_different_reasons_keep_them_apart() {
    let s = Server::new(compile(CONCUR_SRC, None).unwrap()).unwrap();
    let mut hands = Vec::new();
    for t in 0..8 {
        let s = s.clone();
        hands.push(std::thread::spawn(move || {
            for i in 0..50 {
                let want = format!("t{t}-{i}");
                let mut out = Vec::new();
                let (code, _) = s.dispatch("GET", &format!("/named?r={want}"), b"", &mut out);
                let body = String::from_utf8(out).unwrap();
                assert_eq!(code, 400);
                assert_eq!(body, format!("{{\"error\":\"{want}\"}}"), "reason crossed threads");
            }
        }));
    }
    for h in hands {
        h.join().unwrap();
    }
}

const SWEEP_SRC: &str = r#"
GET /count => db.sessions.count()
GET /team  => db.sessions.where("team", query.t).count()
"#;

#[test]
fn expiry_sweeping_beside_live_traffic_keeps_what_it_should() {
    let store = velo::Store::new();
    let s = Server::new(compile(SWEEP_SRC, Some(store.clone())).unwrap()).unwrap();
    let sessions = store.collection("sessions");
    let now = || {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
            as u64
    };
    for i in 0..600 {
        let until = if i % 2 == 0 { now() - 5_000 } else { now() + 600_000 };
        let raw = format!(r#"{{"id":"seed-{i}","until":{until},"team":"t{}"}}"#, i % 3);
        sessions.create(parse_json(raw.as_bytes()).unwrap(), &[]).unwrap();
    }
    let rules = vec![("sessions".to_string(), "until".to_string())];
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen_short = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let sweeper = {
        let (store, rules, stop) = (store.clone(), rules.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut gone = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                gone += store.expire_now(&rules);
            }
            gone
        })
    };
    let writer = {
        let (sessions, stop) = (sessions.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut made = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let raw = format!(
                    r#"{{"id":"live-{made}","until":{},"team":"t{}"}}"#,
                    now() + 600_000,
                    made % 3
                );
                sessions.create(parse_json(raw.as_bytes()).unwrap(), &[]).unwrap();
                made += 1;
            }
            made
        })
    };
    let reader = {
        let (s, stop, seen_short) = (s.clone(), stop.clone(), seen_short.clone());
        std::thread::spawn(move || {
            let mut reads = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let n: i64 = call(&s, "GET", "/count", "").1.parse().unwrap();
                for t in ["t0", "t1", "t2"] {
                    let part: i64 = call(&s, "GET", &format!("/team?t={t}"), "").1.parse().unwrap();
                    assert!(part >= 0, "a filtered count came back negative");
                }
                assert!(n > 0, "the collection emptied while rows were still being written");
                if n < 600 {
                    seen_short.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                reads += 1;
            }
            reads
        })
    };

    std::thread::sleep(Duration::from_millis(700));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let gone = sweeper.join().unwrap();
    let made = writer.join().unwrap();
    let reads = reader.join().unwrap();
    assert!(gone >= 300, "the sweeper never removed the 300 expired rows: {gone}");
    assert!(made > 10 && reads > 10, "writer {made} reader {reads} barely ran");
    assert!(
        seen_short.load(std::sync::atomic::Ordering::Relaxed),
        "the reader never saw the collection shrink, so nothing was concurrent"
    );

    assert_eq!(store.expire_now(&rules), 0, "a final sweep finds nothing left to expire");
    let left: i64 = call(&s, "GET", "/count", "").1.parse().unwrap();
    let by_team: i64 = ["t0", "t1", "t2"]
        .iter()
        .map(|t| call(&s, "GET", &format!("/team?t={t}"), "").1.parse::<i64>().unwrap())
        .sum();
    assert_eq!(by_team, left, "with nothing writing, the parts must add up to the whole");
    assert_eq!(left as u64, 300 + made, "every unexpired row survived, seeded and written alike");
    for i in (1..600).step_by(2) {
        assert!(sessions.find(&format!("seed-{i}")).is_some(), "seed-{i} was swept by mistake");
    }
    for i in (0..600).step_by(2) {
        assert!(sessions.find(&format!("seed-{i}")).is_none(), "seed-{i} should have expired");
    }
}
