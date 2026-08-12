use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;
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
GET  /find            => db.users.search("name", query.q)
GET  /totals          => { n: db.users.count(), sum: db.users.sum("score"), avg: db.users.avg("score"), lo: db.users.min("score"), hi: db.users.max("score") }
GET  /q               => { limit: query.limit, tag: query.tag }
GET  /raw/:v          => { v: v }
DELETE /gone/:id      => { id: id } : 204
POST /accepted        => "queued" : 202
GET  /paged           => db.users.page(query.offset, query.limit)
GET  /byname/:n       => db.users.first("name", n)
POST /keyed           => db.keyed.create(body)
GET  /keyed/:id       => db.keyed.find(id)
GET  /time            => { at: now() }
GET  /id              => uuid()
GET  /sizes           => { s: len("abc"), a: len([1,2]), n: len(null) }
GET  /home            => env("VELO_TEST_ENV")
GET  /sorted          => db.users.order("name")
GET  /rsorted         => db.users.order("-id")
GET  /whoami          => { agent: header.user_agent, auth: header.authorization }
GET  /tenant          => db.users.where("team", header.x_team)
GET  /admin           => db.users.all() when header.authorization == "Bearer secret"
GET  /gated           => "in" when header.x_key
POST /validated       => db.users.create(body) when body.name or 400
GET  /forbidden       => "secret" when header.x_key == "root" or 403
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
    assert!(health.const_text);
    let version = prog.routes.iter().find(|r| r.pattern == "/version").unwrap();
    assert_eq!(version.konst.as_deref(), Some(br#"{"name":"velo","n":2}"#.as_slice()));
    let users =
        prog.routes.iter().find(|r| r.pattern == "/users" && r.method.name() == "GET").unwrap();
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
    assert_eq!(call(&s, "GET", "/echo/1/x/2", "").1, r#"{"a":"1","b":"2"}"#);
    assert_eq!(call(&s, "POST", "/name", r#"{"name":"velo"}"#), (201, "velo".into(), Ctype::Text));
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

#[test]
fn cors_preflight_and_headers() {
    let mut s = Server::new(compile(SRC, None).unwrap()).unwrap();
    {
        let srv = Arc::get_mut(&mut s).unwrap();
        srv.cors = true;
        srv.extra_headers = b"Access-Control-Allow-Origin: *\r\n".to_vec();
    }
    assert_eq!(call(&s, "OPTIONS", "/users", ""), (204, String::new(), Ctype::Json));
    assert_eq!(call(&s, "GET", "/nope", "").0, 404);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let s2 = s.clone();
    std::thread::spawn(move || s2.serve(listener));
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
    assert!(err.starts_with("line 2: unknown identifier"), "{err}");
    assert!(err.contains("2 | GET /users => user.all()"), "{err}");
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
    std::thread::spawn(move || bg.serve(listener));
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
    assert_eq!((status, ct), (200, Ctype::Json));
    let m = velo::value::parse_json(body.as_bytes()).unwrap();
    assert!(matches!(m.get("version"), Value::Str(_)), "{body}");
    assert!(matches!(m.get("requests"), Value::Num(n) if n >= 4.0), "{body}");
    assert!(matches!(m.get("failures"), Value::Num(n) if n == 2.0), "{body}");
    assert!(matches!(m.get("routes"), Value::Num(n) if n > 0.0), "{body}");

    let plain = server();
    assert_eq!(call(&plain, "GET", "/_metrics", "").0, 404);
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
    std::thread::spawn(move || bg.serve(listener));

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
    std::thread::spawn(move || bg.serve(listener));
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

    assert!(compile("GET /a => 1 when 1 or 99", None).is_err());
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
    std::thread::spawn(move || bg.serve(listener));

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
    assert!(!docs.const_text);

    let s = Server::new(prog).unwrap();
    let (status, body, ct) = call(&s, "GET", "/docs", "");
    assert_eq!((status, ct), (200, Ctype::Json));
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
    std::thread::spawn(move || bg.serve(listener));

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
