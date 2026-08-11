use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use velo::http::Ctype;
use velo::value::parse_json;
use velo::{compile, Server, Value};

const SRC: &str = r#"
GET  /health          => "ok"
GET  /version         => { name: "velo", n: 2 }
GET  /users           => db.users.all()
GET  /users/:id       => db.users.find(id)
POST /users           => db.users.create(body)
PUT  /users/:id       => db.users.update(id, body)
DELETE /users/:id     => db.users.delete(id)
GET  /stats           => { users: db.users.count() }
GET  /echo/:a/x/:b    => { a: a, b: b }
POST /name            => body.name
GET  /list            => [1, 2, "three", true, null]
GET  /search          => db.users.where("name", query.name)
GET  /q               => { limit: query.limit, tag: query.tag }
GET  /raw/:v          => { v: v }
DELETE /gone/:id      => { id: id } : 204
POST /accepted        => "queued" : 202
GET  /paged           => db.users.page(query.offset, query.limit)
GET  /time            => { at: now() }
GET  /id              => uuid()
GET  /sizes           => { s: len("abc"), a: len([1,2]), n: len(null) }
GET  /home            => env("VELO_TEST_ENV")
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
    assert!(health.const_text);
    let version = prog.routes.iter().find(|r| r.pattern == "/version").unwrap();
    assert_eq!(
        version.konst.as_deref(),
        Some(br#"{"name":"velo","n":2}"#.as_slice())
    );
    let users = prog.routes.iter().find(|r| r.pattern == "/users" && r.method.name() == "GET").unwrap();
    assert!(users.konst.is_none());
}

#[test]
fn crud_roundtrip() {
    let s = server();
    assert_eq!(call(&s, "GET", "/users", ""), (200, "[]".into(), Ctype::Json));

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
    assert_eq!(
        call(&s, "GET", "/echo/1/x/2", "").1,
        r#"{"a":"1","b":"2"}"#
    );
    assert_eq!(
        call(&s, "POST", "/name", r#"{"name":"velo"}"#),
        (201, "velo".into(), Ctype::Text)
    );
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
    assert_eq!(
        String::from_utf8(v.to_json()).unwrap(),
        "\"a\\\"b\\\\c\\nd\\te\\u0001\""
    );
}

fn spawn() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = server();
    std::thread::spawn(move || {
        let _ = s.serve(listener);
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
                let res = raw(port, b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
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
    assert_eq!(
        call(&s, "GET", "/q?limit=10&tag=a+b", "").1,
        r#"{"limit":"10","tag":"a b"}"#
    );
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
    assert_eq!(
        body,
        r#"[{"id":1,"name":"a","team":"x"},{"id":3,"name":"a","team":"z"}]"#
    );
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
    assert_eq!(call(&s, "GET", "/paged?offset=1&limit=2", "").1, r#"[{"id":2,"n":1},{"id":3,"n":2}]"#);
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
    let h = std::thread::spawn(move || s2.serve(listener));
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
    let at = body.trim_start_matches("{\"at\":").trim_end_matches('}').parse::<u64>().unwrap();
    assert!(at > 1_700_000_000_000, "{at}");

    let (status, ev, _) = call(&s, "POST", "/events", r#"{"kind":"ping"}"#);
    assert_eq!(status, 201);
    assert!(ev.contains(r#""data":{"kind":"ping"}"#), "{ev}");

    assert!(compile("GET /a => nope()", None).is_err());
    assert!(compile("GET /a => len()", None).is_err());
    assert!(compile("GET /a => now(1)", None).is_err());
}
