use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_velo");

fn watchdog() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let limit: u64 =
            std::env::var("VELO_TEST_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(limit));
            let note = format!(
                "\nvelo tests: still running after {limit}s, something is waiting forever\n"
            );
            let _ = std::io::Write::write_all(&mut std::io::stderr(), note.as_bytes());
            std::process::exit(101);
        });
    });
}

fn tmp(name: &str) -> std::path::PathBuf {
    watchdog();
    std::env::temp_dir().join(format!("velo-cli-{}-{name}", std::process::id()))
}

fn write(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

fn free_port() -> u16 {
    watchdog();
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn request(port: u16, req: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", port)) {
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            if c.write_all(req.as_bytes()).is_ok() {
                let mut out = String::new();
                if c.read_to_string(&mut out).is_ok() && !out.is_empty() {
                    return out;
                }
            }
        }
        assert!(Instant::now() < deadline, "server never answered on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn get(port: u16, path: &str) -> String {
    request(port, &format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"))
}

fn post(port: u16, path: &str, body: &str) -> String {
    request(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn patch(port: u16, path: &str, body: &str) -> String {
    request(
        port,
        &format!(
            "PATCH {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn stop(child: &mut Child, signal: &str) {
    let _ = Command::new("kill").arg(signal).arg(child.id().to_string()).status();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process ignored {signal}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn check_reports_errors_with_exit_codes() {
    let good = tmp("good.velo");
    write(&good, "GET /health => \"ok\"\n");
    let out = Command::new(BIN).arg("check").arg(&good).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 routes"));

    let bad = tmp("bad.velo");
    write(&bad, "GET /health => \"ok\"\nGET /users => user.all()\n");
    let out = Command::new(BIN).arg("check").arg(&bad).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("line 2:15: unknown identifier"), "{err}");
    assert!(err.contains("2 | GET /users => user.all()"), "{err}");
    assert!(err.contains("|               ^"), "caret missing in {err}");

    let missing = Command::new(BIN).arg("check").arg(tmp("nope.velo")).output().unwrap();
    assert_eq!(missing.status.code(), Some(1));

    let _ = std::fs::remove_file(&good);
    let _ = std::fs::remove_file(&bad);
}

#[test]
fn new_scaffolds_a_runnable_file() {
    let path = tmp("scaffold.velo");
    let _ = std::fs::remove_file(&path);
    assert!(Command::new(BIN).arg("new").arg(&path).status().unwrap().success());

    let again = Command::new(BIN).arg("new").arg(&path).output().unwrap();
    assert_eq!(again.status.code(), Some(1), "should refuse to overwrite");

    let out = Command::new(BIN).arg("check").arg(&path).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn openapi_command_prints_a_document() {
    let path = tmp("doc.velo");
    write(&path, "GET /users/:id => db.users.find(id)\nPOST /users => db.users.create(body)\n");
    let out = Command::new(BIN).arg("openapi").arg(&path).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let doc = velo::value::parse_json(text.trim().as_bytes()).expect(&text);
    assert_eq!(doc.get("openapi").as_key(), "3.0.3");
    let expected = path.file_stem().unwrap().to_string_lossy().to_string();
    assert_eq!(doc.get("info").get("title").as_key(), expected);
    assert!(text.contains("/users/{id}"), "{text}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn run_persists_across_a_sigterm_restart() {
    let app = tmp("persist.velo");
    let data = tmp("persist.json");
    let _ = std::fs::remove_file(&data);
    write(
        &app,
        "GET /health => \"ok\"\nGET /users => db.users.all()\nPOST /users => db.users.create(body)\n",
    );
    let port = free_port();

    let spawn = || {
        Command::new(BIN)
            .arg("run")
            .arg(&app)
            .arg(format!("127.0.0.1:{port}"))
            .arg("--data")
            .arg(&data)
            .env("VELO_SAVE_MS", "50")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };

    let mut child = spawn();
    assert!(get(port, "/health").ends_with("ok"));
    assert!(post(port, "/users", r#"{"name":"kept"}"#).contains(r#""id":1"#));
    stop(&mut child, "-TERM");

    let saved = std::fs::read_to_string(&data).unwrap();
    assert!(saved.contains("kept"), "{saved}");

    let mut child = spawn();
    let body = get(port, "/users");
    assert!(body.contains(r#""name":"kept""#), "{body}");
    assert!(post(port, "/users", r#"{"name":"second"}"#).contains(r#""id":2"#));
    stop(&mut child, "-INT");

    let saved = std::fs::read_to_string(&data).unwrap();
    assert!(saved.contains("second"), "{saved}");
    let _ = std::fs::remove_file(&app);
    let _ = std::fs::remove_file(&data);
}

#[test]
fn run_serves_metrics_when_configured() {
    let app = tmp("metrics.velo");
    write(&app, "GET /health => \"ok\"\n");
    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(&app)
        .arg(format!("127.0.0.1:{port}"))
        .env("VELO_METRICS", "/_metrics")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    assert!(get(port, "/health").ends_with("ok"));
    let res = get(port, "/_metrics");
    let body = res.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
    let m = velo::value::parse_json(body.as_bytes()).expect(&res);
    assert_eq!(m.get("version").as_key(), velo::VERSION);
    assert!(matches!(m.get("requests"), velo::Value::Num(n) if n >= 2.0), "{body}");
    assert!(matches!(m.get("workers"), velo::Value::Num(n) if n >= 1.0), "{body}");

    stop(&mut child, "-TERM");
    let _ = std::fs::remove_file(&app);
}

#[test]
fn metrics_track_latency_and_bytes() {
    let app = tmp("latency.velo");
    write(&app, "GET /health => \"ok\"\nGET /list => [1,2,3]\n");
    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(&app)
        .arg(format!("127.0.0.1:{port}"))
        .env("VELO_METRICS", "/_metrics")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    for _ in 0..20 {
        assert!(get(port, "/list").ends_with("[1,2,3]"));
    }
    let res = get(port, "/_metrics");
    let body = res.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
    let m = velo::value::parse_json(body.as_bytes()).expect(&res);
    let num = |k: &str| match m.get(k) {
        velo::Value::Num(n) => n,
        _ => panic!("{k} missing in {body}"),
    };
    assert!(num("bytes_out") >= 20.0 * 7.0, "{body}");
    assert!(num("max_micros") >= num("avg_micros"), "{body}");
    assert!(num("max_micros") < 1_000_000.0, "{body}");

    stop(&mut child, "-TERM");
    let _ = std::fs::remove_file(&app);
}

#[test]
fn includes_merge_files_and_ignore_cycles() {
    let dir = tmp("inc");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    write(
        &dir.join("app.velo"),
        "include \"parts/users.velo\"\ninclude \"parts/posts.velo\"\n\nGET /health => \"ok\"\n",
    );
    write(
        &dir.join("parts/users.velo"),
        "GET /users => db.users.all()\nPOST /users => db.users.create(body)\n",
    );
    write(&dir.join("parts/posts.velo"), "GET /posts => db.posts.all()\ninclude \"users.velo\"\n");

    let out = Command::new(BIN).arg("routes").arg(dir.join("app.velo")).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(text.lines().count(), 4, "{text}");
    for path in ["/health", "/users", "/posts"] {
        assert!(text.contains(path), "{text}");
    }
    assert!(text.contains("parts/users.velo:1"), "route origins missing in {text}");
    assert!(text.contains("app.velo:4"), "route origins missing in {text}");

    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(dir.join("app.velo"))
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(get(port, "/health").ends_with("ok"));
    assert!(post(port, "/users", r#"{"name":"a"}"#).contains(r#""id":1"#));
    assert!(get(port, "/posts").ends_with("[]"));
    stop(&mut child, "-TERM");

    write(&dir.join("broken.velo"), "include \"nope.velo\"\nGET /a => 1\n");
    let out = Command::new(BIN).arg("check").arg(dir.join("broken.velo")).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope.velo"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn openapi_covers_included_files() {
    let dir = tmp("incdoc");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("app.velo"),
        "include \"more.velo\"\nGET /docs => openapi()\nGET /health => \"ok\"\n",
    );
    write(&dir.join("more.velo"), "GET /users/:id => db.users.find(id)\n");

    let out = Command::new(BIN).arg("openapi").arg(dir.join("app.velo")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("/users/{id}"), "{text}");

    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(dir.join("app.velo"))
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let res = get(port, "/docs");
    let body = res.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
    let doc = velo::value::parse_json(body.as_bytes()).expect(&res);
    let paths = doc.get("paths");
    for p in ["/users/{id}", "/health", "/docs"] {
        assert!(matches!(paths.get(p), velo::Value::Obj(_)), "{p} missing from {body}");
    }
    stop(&mut child, "-TERM");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn watch_restarts_on_change() {
    let app = tmp("watch.velo");
    write(&app, "GET /health => \"ok\"\nGET /v => \"one\"\n");
    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(&app)
        .arg(format!("127.0.0.1:{port}"))
        .arg("--watch")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    assert!(get(port, "/v").ends_with("one"));

    write(&app, "GET /health => \"ok\"\nGET /v => \"two\"\n");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if get(port, "/v").ends_with("two") {
            break;
        }
        assert!(Instant::now() < deadline, "watch never picked up the change");
        std::thread::sleep(Duration::from_millis(200));
    }

    write(&app, "GET /v => oops(\n");
    std::thread::sleep(Duration::from_millis(1200));
    write(&app, "GET /health => \"ok\"\nGET /v => \"three\"\n");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if get(port, "/v").ends_with("three") {
            break;
        }
        assert!(Instant::now() < deadline, "watch died on a broken file");
        std::thread::sleep(Duration::from_millis(200));
    }

    stop(&mut child, "-TERM");
    let _ = std::fs::remove_file(&app);
}

#[test]
fn file_builtin_serves_a_page_with_its_type() {
    let dir = tmp("static");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(&dir.join("index.html"), "<h1>hello</h1>\n");
    write(&dir.join("style.css"), "body { color: red }\n");
    write(&dir.join("app.velo"), "GET /health => \"ok\"\nGET / => file(\"index.html\")\nGET /style.css => file(\"style.css\")\n");

    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(dir.join("app.velo"))
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let page = get(port, "/");
    assert!(page.contains("Content-Type: text/html; charset=utf-8"), "{page}");
    assert!(page.ends_with("<h1>hello</h1>\n"), "{page}");
    let css = get(port, "/style.css");
    assert!(css.contains("Content-Type: text/css; charset=utf-8"), "{css}");
    assert!(get(port, "/health").ends_with("ok"));
    stop(&mut child, "-TERM");

    write(&dir.join("bad.velo"), "GET / => file(\"missing.html\")\n");
    let out = Command::new(BIN).arg("check").arg(dir.join("bad.velo")).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing.html"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn logs_carry_status_bytes_and_duration() {
    let app = tmp("log.velo");
    write(&app, "GET /health => \"ok\"\nGET /list => [1,2,3]\n");

    for (mode, check) in [("1", "text"), ("json", "json")] {
        let port = free_port();
        let log = tmp(&format!("log-{mode}.txt"));
        let file = std::fs::File::create(&log).unwrap();
        let mut child = Command::new(BIN)
            .arg("run")
            .arg(&app)
            .arg(format!("127.0.0.1:{port}"))
            .env("VELO_LOG", mode)
            .stdout(Stdio::null())
            .stderr(Stdio::from(file))
            .spawn()
            .unwrap();

        assert!(get(port, "/list").ends_with("[1,2,3]"));
        assert_eq!(get(port, "/missing").lines().next().unwrap(), "HTTP/1.1 404 Not Found");

        let want = if check == "json" { "\"status\":404" } else { "GET /missing 404 " };
        let deadline = Instant::now() + Duration::from_secs(15);
        let text = loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains(want) {
                break text;
            }
            assert!(Instant::now() < deadline, "{mode} log never reached the disk: {text:?}");
            std::thread::sleep(Duration::from_millis(50));
        };
        stop(&mut child, "-TERM");
        if check == "json" {
            let line = text.lines().find(|l| l.contains("\"/list\"")).unwrap_or_default();
            let v = velo::value::parse_json(line.as_bytes()).expect(&text);
            assert_eq!(v.get("method").as_key(), "GET");
            assert_eq!(v.get("status").as_key(), "200");
            assert_eq!(v.get("bytes").as_key(), "7");
            assert!(matches!(v.get("micros"), velo::Value::Num(n) if n >= 0.0), "{text}");
            assert!(text.lines().any(|l| l.contains("\"status\":404")), "{text}");
        } else {
            assert!(text.lines().any(|l| l.starts_with("GET /list 200 7b ")), "{text}");
            assert!(text.lines().any(|l| l.contains("GET /missing 404 ")), "{text}");
        }
        let _ = std::fs::remove_file(&log);
    }
    let _ = std::fs::remove_file(&app);
}

#[test]
fn serves_on_a_unix_socket() {
    use std::os::unix::net::UnixStream;

    let app = tmp("unix.velo");
    let sock = tmp("velo.sock");
    let _ = std::fs::remove_file(&sock);
    write(&app, "GET /health => \"ok\"\nPOST /users => db.users.create(body)\n");

    let mut child = Command::new(BIN)
        .arg("run")
        .arg(&app)
        .arg(format!("unix:{}", sock.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut answer = String::new();
    while Instant::now() < deadline {
        if let Ok(mut c) = UnixStream::connect(&sock) {
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            c.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
            answer.clear();
            if c.read_to_string(&mut answer).is_ok() && !answer.is_empty() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(answer.ends_with("ok"), "{answer}");

    let mut c = UnixStream::connect(&sock).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    c.write_all(b"POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"name\":\"mark\"}").unwrap();
    let mut created = String::new();
    c.read_to_string(&mut created).unwrap();
    assert!(created.starts_with("HTTP/1.1 201"), "{created}");

    stop(&mut child, "-TERM");
    assert!(!sock.exists(), "socket file left behind");
    let _ = std::fs::remove_file(&app);
}

#[test]
fn documented_knobs_and_operations_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();

    let mut sources = String::new();
    for dir in ["src", "src/bin", "tests"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
            if entry.path().extension().is_some_and(|e| e == "rs") {
                sources.push_str(&std::fs::read_to_string(entry.path()).unwrap());
            }
        }
    }

    let mut documented: Vec<String> = Vec::new();
    let mut rest = readme.as_str();
    while let Some(at) = rest.find("`VELO_") {
        rest = &rest[at + 1..];
        let name: String =
            rest.chars().take_while(|c| c.is_ascii_uppercase() || *c == '_').collect();
        if !documented.contains(&name) {
            documented.push(name);
        }
    }
    assert!(documented.len() > 10, "README lost its environment table");
    for name in &documented {
        assert!(sources.contains(name.as_str()), "{name} is documented but not used anywhere");
    }

    let program = "\
GET /a1 => db.x.all()\n\
GET /a2 => db.x.count()\n\
GET /a3 => db.x.find(\"1\")\n\
GET /a4 => db.x.first(\"k\", \"v\")\n\
GET /a5 => db.x.where(\"k\", \"v\")\n\
GET /a6 => db.x.search(\"k\", \"v\")\n\
GET /a7 => db.x.order(\"k\")\n\
GET /a8 => db.x.page(0, 10)\n\
GET /a9 => { s: db.x.sum(\"n\"), a: db.x.avg(\"n\"), l: db.x.min(\"n\"), h: db.x.max(\"n\") }\n\
POST /a10 => db.x.create(body)\n\
PUT /a11/:id => db.x.update(id, body)\n\
PUT /a12/:id => db.x.upsert(id, body)\n\
DELETE /a13/:id => db.x.delete(id)\n\
DELETE /a14 => db.x.delete_where(\"k\", \"v\")\n\
DELETE /a15 => db.x.clear()\n\
GET /b1 => { now: now(), id: uuid(), n: len(\"abc\"), home: env(\"HOME\"), at: date(now()) }\n\
GET /b3 => { d: default(query.q, \"x\"), l: lower(\"A\"), u: upper(\"a\"), t: trim(\" a \") }\n\
GET /b2 => openapi()\n\
GET /c1 => { m: 1 + 2 * 3, d: 10 / 4, cmp: 2 < 3, both: 1 == 1 and 2 != 3 }\n\
GET /c2/:id => { id: id, q: query.z, h: header.x_test, doubled: id * 2 } : 200\n\
POST /c3 => body.name when body.name and header.x_key else 400\n\
GET /d1 => db.x.where(\"k\", \"v\").order(\"-n\").page(0, 20)\n\
GET /d2 => db.x.search(\"k\", \"v\").count()\n\
GET /d3 => db.x.where(\"k\", \"v\").sum(\"n\")\n\
GET /d4 => db.x.where(\"k\", \"v\").order(\"n\").first()\n\
GET /d5 => db.x.where(\"n\", \">=\", 10).where(\"n\", \"<\", 100).count()\n\
GET /d6 => db.x.where(\"k\", \"v\").select(\"id\", \"k\")\n\
POST /e1 => db.x.create({ p: password(body.pass), fp: hash(body.pass) })\n\
POST /e2 => \"in\" when verify(body.pass, db.x.find(body.user).p) else 401\n\
GET /e3 => db.x.find(cookie.session)\n\
POST /e4 => setcookie(\"session\", uuid())\n\
GET /e5/:id => db.x.find(id).select(\"id\", \"k\")\n\
POST /e6 => db.x.create(body.select(\"k\", \"n\")) when body.k else 400 \"k is required\"\n\
DELETE /e7 => db.x.delete_where(\"n\", \"<\", 5)\n\
GET /e8 => \"ok\" when limit(header.x_key, 5) else 401\n\
POST /e9 => \"ok\" when check(body.k, \"k is required\")\n";

    let path = tmp("everything.velo");
    write(&path, program);
    let out = Command::new(BIN).arg("check").arg(&path).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("36 routes"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn watch_follows_files_pulled_in_by_file() {
    let dir = tmp("watchassets");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(&dir.join("page.html"), "<h1>one</h1>\n");
    write(&dir.join("app.velo"), "GET /health => \"ok\"\nGET / => file(\"page.html\")\n");

    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(dir.join("app.velo"))
        .arg(format!("127.0.0.1:{port}"))
        .arg("--watch")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    assert!(get(port, "/").ends_with("<h1>one</h1>\n"));

    write(&dir.join("page.html"), "<h1>two</h1>\n");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if get(port, "/").ends_with("<h1>two</h1>\n") {
            break;
        }
        assert!(Instant::now() < deadline, "watch ignored the html file");
        std::thread::sleep(Duration::from_millis(200));
    }

    stop(&mut child, "-TERM");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn including_a_directory_takes_every_velo_file_in_name_order() {
    let dir = tmp("incdir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("parts/nested")).unwrap();
    write(&dir.join("app.velo"), "include \"parts\"\n\nGET /health => \"ok\"\n");
    write(&dir.join("parts/b_posts.velo"), "GET /posts => db.posts.all()\n");
    write(&dir.join("parts/a_users.velo"), "GET /users => db.users.all()\n");
    write(&dir.join("parts/notes.txt"), "GET /nope => \"no\"\n");
    write(&dir.join("parts/readme.md"), "not velo\n");
    write(&dir.join("parts/nested/deep.velo"), "GET /deep => \"deep\"\n");

    let out = Command::new(BIN).arg("routes").arg(dir.join("app.velo")).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(text.lines().count(), 3, "only the two velo files and app itself: {text}");
    assert!(text.contains("/users") && text.contains("/posts") && text.contains("/health"));
    assert!(!text.contains("/deep"), "a directory include does not recurse: {text}");
    assert!(!text.contains("/nope"), "a file that is not .velo is left alone: {text}");
    let users = text.find("/users").unwrap();
    let posts = text.find("/posts").unwrap();
    assert!(users < posts, "files load in name order, so a_users before b_posts: {text}");

    write(&dir.join("both.velo"), "include \"parts\"\ninclude \"parts/a_users.velo\"\n");
    let out = Command::new(BIN).arg("check").arg(dir.join("both.velo")).output().unwrap();
    assert!(
        out.status.success(),
        "a file reached twice must not be loaded twice: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 routes"));

    write(&dir.join("missing.velo"), "include \"gone\"\nGET /a => \"a\"\n");
    let out = Command::new(BIN).arg("check").arg(dir.join("missing.velo")).output().unwrap();
    assert!(!out.status.success(), "a missing include must fail");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("gone"), "the error names the path it could not read: {err}");

    std::fs::create_dir_all(dir.join("empty")).unwrap();
    write(&dir.join("bare.velo"), "include \"empty\"\nGET /only => \"one\"\n");
    let out = Command::new(BIN).arg("check").arg(dir.join("bare.velo")).output().unwrap();
    assert!(
        out.status.success(),
        "an empty directory is not an error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn watch_notices_a_file_appearing_in_an_included_directory() {
    let dir = tmp("watchdir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    write(&dir.join("app.velo"), "include \"parts\"\nGET /health => \"ok\"\n");
    write(&dir.join("parts/one.velo"), "GET /one => \"first\"\n");

    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(dir.join("app.velo"))
        .arg(format!("127.0.0.1:{port}"))
        .arg("--watch")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    assert!(get(port, "/one").ends_with("first"), "{}", get(port, "/one"));
    assert!(get(port, "/two").contains("not found"), "the second route is not there yet");

    write(&dir.join("parts/two.velo"), "GET /two => \"second\"\n");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if get(port, "/two").ends_with("second") {
            break;
        }
        assert!(Instant::now() < deadline, "watch never picked up the new file in the directory");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(get(port, "/one").ends_with("first"), "the file that was already there still serves");

    stop(&mut child, "-TERM");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_sweep_runs_while_the_server_serves() {
    let dir = tmp("sweep");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("app.velo"),
        "POST /add   => db.sessions.create(body)\n\
         GET  /count => db.sessions.count()\n\
         GET  /live  => db.sessions.where(\"kind\", \"live\").count()\n",
    );

    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(dir.join("app.velo"))
        .arg(format!("127.0.0.1:{port}"))
        .env("VELO_EXPIRE", "sessions.until")
        .env("VELO_EXPIRE_MS", "100")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let now = || {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
            as u64
    };
    for i in 0..40 {
        let until = if i % 2 == 0 { now() + 400 } else { now() + 600_000 };
        let kind = if i % 2 == 0 { "short" } else { "live" };
        post(port, "/add", &format!(r#"{{"id":"s{i}","until":{until},"kind":"{kind}"}}"#));
    }
    let body = |path: &str| {
        let res = get(port, path);
        res.rsplit("\r\n\r\n").next().unwrap_or_default().to_string()
    };
    assert_eq!(body("/count"), "40");

    let deadline = Instant::now() + Duration::from_secs(20);
    while body("/count") != "20" {
        assert!(Instant::now() < deadline, "the sweep never ran: {}", body("/count"));
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(body("/live"), "20", "it took a row it should not have");

    post(port, "/add", &format!(r#"{{"id":"late","until":{},"kind":"live"}}"#, now() + 600_000));
    post(port, "/add", &format!(r#"{{"id":"doomed","until":{},"kind":"short"}}"#, now() + 300));
    assert_eq!(body("/count"), "22", "both writes landed");
    let deadline = Instant::now() + Duration::from_secs(20);
    while body("/count") != "21" {
        assert!(Instant::now() < deadline, "the sweep stopped after the first pass");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(body("/live"), "21", "the row written after the first sweep is still there");
    stop(&mut child, "-TERM");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bench_loads_the_routes_it_can_and_says_why_it_skips_the_rest() {
    let dir = tmp("bench");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("app.velo"),
        "GET /health     => \"ok\"\n\
         GET /users      => db.users.all()\n\
         GET /count      => db.users.count()\n\
         GET /users/:id  => db.users.find(id)\n\
         POST /users     => db.users.create(body)\n\
         GET /gated      => \"secret\" when header.x_key else 403\n",
    );
    std::fs::write(
        dir.join("data.json"),
        r#"{"users":{"next_id":2,"rows":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}}"#,
    )
    .unwrap();

    let out = Command::new(BIN)
        .arg("bench")
        .arg(dir.join("app.velo"))
        .args(["-c", "2", "-d", "1", "--data"])
        .arg(dir.join("data.json"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("benching 3 route(s)"), "{text}");
    assert!(text.contains("2 row(s) loaded"), "the data file was not read: {text}");
    assert!(!text.contains("the store is empty"), "{text}");
    for path in ["GET /health", "GET /users", "GET /count"] {
        assert!(text.contains(path), "{path} missing from {text}");
    }
    assert!(text.contains("GET /users/:id"), "{text}");
    assert!(text.contains("needs a path parameter"), "{text}");
    assert!(text.contains("POST /users") && text.contains("not a GET"), "{text}");
    assert!(text.contains("GET /gated") && text.contains("behind a guard"), "{text}");
    assert!(text.contains("req/s") && text.contains("p99"), "{text}");
    let rates: Vec<f64> = text
        .lines()
        .filter_map(|l| l.split_whitespace().find(|w| w.parse::<f64>().is_ok()))
        .filter_map(|w| w.parse().ok())
        .collect();
    assert!(rates.len() >= 3, "no throughput numbers in {text}");
    assert!(rates.windows(2).all(|w| w[0] <= w[1]), "slowest route first: {rates:?}");

    let out = Command::new(BIN)
        .arg("bench")
        .arg(dir.join("app.velo"))
        .args(["-c", "2", "-d", "1"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("the store is empty"), "no warning");

    write(&dir.join("all.velo"), "GET /a/:id => db.x.find(id)\n");
    let out = Command::new(BIN).arg("bench").arg(dir.join("all.velo")).output().unwrap();
    assert!(!out.status.success(), "nothing benchable must fail");
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing to bench"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_wal_survives_a_kill_that_the_snapshot_does_not() {
    let app = tmp("wal.velo");
    let data = tmp("wal-data.json");
    let log = tmp("wal-data.log");
    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_file(&log);
    write(
        &app,
        "POST /users => db.users.create(body)\n\
         GET /users => db.users.all()\n\
         PATCH /users/:id => db.users.update(id, body) : 200\n\
         PUT /users/:id => db.users.upsert(id, body)\n\
         POST /bump/:id => db.users.incr(id, \"n\") : 200\n\
         DELETE /users/:id => db.users.delete(id) : 204\n\
         DELETE /old => db.users.delete_where(\"team\", \"gone\")\n",
    );
    let port = free_port();
    let boot = |port: u16| {
        Command::new(BIN)
            .arg("run")
            .arg(&app)
            .arg(format!("127.0.0.1:{port}"))
            .arg("--data")
            .arg(&data)
            .arg("--wal")
            .arg(&log)
            .env("VELO_SAVE_MS", "600000")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut child = boot(port);
    post(port, "/users", r#"{"id":"a","name":"a"}"#);
    post(port, "/users", r#"{"id":"b","name":"b","team":"gone"}"#);
    post(port, "/users", r#"{"id":"c","name":"c"}"#);
    request(
        port,
        &format!(
            "PUT /users/a HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            r#"{"name":"A"}"#.len(),
            r#"{"name":"A"}"#
        ),
    );
    post(port, "/bump/a", "");
    post(port, "/bump/a", "");
    post(port, "/users", r#"{"id":"n","profile":{"city":"bkk","font":"sans"}}"#);
    patch(port, "/users/n", r#"{"profile":{"city":"cnx"}}"#);
    request(port, "DELETE /users/c HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    request(port, "DELETE /old HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    let before = get(port, "/users");
    let before = before.split("\r\n\r\n").nth(1).unwrap().to_string();
    assert!(before.contains(r#""n":2"#), "{before}");
    assert!(before.contains(r#""font":"sans""#), "a deep patch kept the sibling: {before}");
    assert!(before.contains(r#""city":"cnx""#), "{before}");

    let _ = Command::new("kill").arg("-9").arg(child.id().to_string()).status();
    let _ = child.wait();
    assert!(!data.exists(), "no snapshot should have been written yet");

    let port = free_port();
    let mut child = boot(port);
    let after = get(port, "/users");
    let after = after.split("\r\n\r\n").nth(1).unwrap().to_string();
    assert_eq!(after, before, "a hard kill must lose nothing with a wal");
    stop(&mut child, "-TERM");

    let left = std::fs::metadata(&log).unwrap().len();
    assert_eq!(left, 0, "a clean stop snapshots and empties the wal");
    assert!(data.exists());

    let port = free_port();
    let mut child = boot(port);
    let again = get(port, "/users");
    let again = again.split("\r\n\r\n").nth(1).unwrap().to_string();
    assert_eq!(again, before, "the snapshot alone must carry the same rows");
    stop(&mut child, "-TERM");
    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_file(&log);
}

#[test]
fn a_snapshot_leaves_only_the_writes_that_followed_it_in_the_wal() {
    const ROWS: usize = 300;
    let app = tmp("wal2.velo");
    let data = tmp("wal2-data.json");
    let log = tmp("wal2-data.log");
    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_file(&log);
    write(&app, "POST /users => db.users.create(body)\nGET /count => db.users.count()\n");
    let boot = |port: u16, every: &str| {
        Command::new(BIN)
            .arg("run")
            .arg(&app)
            .arg(format!("127.0.0.1:{port}"))
            .arg("--data")
            .arg(&data)
            .arg("--wal")
            .arg(&log)
            .env("VELO_SAVE_MS", every)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let port = free_port();
    let mut child = boot(port, "20");
    for i in 0..ROWS {
        post(port, "/users", &format!(r#"{{"id":"u{i}","n":{i}}}"#));
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let saved = std::fs::metadata(&data).map(|m| m.len()).unwrap_or(0);
        let logged = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(u64::MAX);
        if saved > 0 && logged < saved {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a snapshot never trimmed the wal it covers: {logged} logged, {saved} saved"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = Command::new("kill").arg("-9").arg(child.id().to_string()).status();
    let _ = child.wait();

    let port = free_port();
    let mut child = boot(port, "600000");
    let count = get(port, "/count");
    assert!(
        count.ends_with(&ROWS.to_string()),
        "writes that landed while a snapshot was being written must survive: {count}"
    );
    stop(&mut child, "-TERM");
    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_file(&log);
}

#[test]
fn one_process_owns_a_data_file() {
    let app = tmp("lock.velo");
    let data = tmp("lock-data.json");
    let lock = tmp("lock-data.json.lock");
    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_file(&lock);
    write(&app, "GET /health => \"ok\"\nPOST /users => db.users.create(body)\n");
    let port = free_port();
    let mut child = Command::new(BIN)
        .arg("run")
        .arg(&app)
        .arg(format!("127.0.0.1:{port}"))
        .arg("--data")
        .arg(&data)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(get(port, "/health").ends_with("ok"));

    let mut second = Command::new(BIN)
        .arg("run")
        .arg(&app)
        .arg(format!("127.0.0.1:{}", free_port()))
        .arg("--data")
        .arg(&data)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let gave_up = loop {
        match second.try_wait().unwrap() {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                let _ = second.kill();
                let _ = second.wait();
                panic!("a second owner started instead of refusing");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    assert!(!gave_up.success(), "a second owner must refuse to start");
    let mut said = String::new();
    second.stderr.take().unwrap().read_to_string(&mut said).unwrap();
    assert!(said.contains("held by another velo"), "{said}");
    assert!(get(port, "/health").ends_with("ok"), "the owner keeps serving");

    let elsewhere = tmp("lock-other.json");
    let _ = std::fs::remove_file(&elsewhere);
    let other_port = free_port();
    let mut other = Command::new(BIN)
        .arg("run")
        .arg(&app)
        .arg(format!("127.0.0.1:{other_port}"))
        .arg("--data")
        .arg(&elsewhere)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(get(other_port, "/health").ends_with("ok"), "another data file is not blocked");
    stop(&mut other, "-TERM");
    stop(&mut child, "-TERM");

    let port = free_port();
    let mut again = Command::new(BIN)
        .arg("run")
        .arg(&app)
        .arg(format!("127.0.0.1:{port}"))
        .arg("--data")
        .arg(&data)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(get(port, "/health").ends_with("ok"), "the lock goes when the owner does");
    stop(&mut again, "-TERM");
    for path in [&data, &lock, &elsewhere, &tmp("lock-other.json.lock")] {
        let _ = std::fs::remove_file(path);
    }
}
