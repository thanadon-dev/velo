use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_velo");

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("velo-cli-{}-{name}", std::process::id()))
}

fn write(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

fn free_port() -> u16 {
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
    assert!(err.contains("line 2: unknown identifier"), "{err}");
    assert!(err.contains("2 | GET /users => user.all()"), "{err}");

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
