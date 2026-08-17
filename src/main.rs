use std::cmp::Ordering;
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;
use velo::{compile_file, Server, Store, VERSION};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    match cmd {
        "run" if args.iter().any(|a| a == "--watch") => watch(&args),
        "run" => run(&args),
        "check" => check(&args),
        "routes" => routes(&args),
        "bench" => bench(&args),
        "openapi" => openapi(&args),
        "new" => new(&args),
        "version" | "--version" | "-v" => println!("velo {VERSION}"),
        "help" | "--help" | "-h" | "" => usage(0),
        other => {
            eprintln!("velo: unknown command {other:?}");
            usage(2);
        }
    }
}

fn usage(code: i32) -> ! {
    eprintln!(
        "velo {VERSION}\n\
         \n\
         usage:\n\
         \x20 velo run <file.velo> [addr] [--data file.json] [--wal file.log] [--watch]\n\
         \x20                               start the server (default :8080, env VELO_ADDR)\n\
         \x20 velo check <file.velo>        compile only, report errors\n\
         \x20 velo routes <file.velo>       list compiled routes\n\
         \x20 velo bench <file.velo> [-c n] [-d secs] [-H header] [-b body] [-q query] [--data file.json]\n\
         \x20                               serve the file and load every plain GET route\n\
         \x20 velo openapi <file.velo>      print an OpenAPI 3 document\n\
         \x20 velo new <file.velo>          write a starter file\n\
         \x20 velo version                  print version"
    );
    exit(code)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn program(args: &[String], store: Option<std::sync::Arc<Store>>) -> velo::Program {
    let Some(path) = args.get(2) else { usage(2) };
    match compile_file(std::path::Path::new(path), store) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    }
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn watch(args: &[String]) {
    let child_args: Vec<String> =
        args.iter().skip(1).filter(|a| a.as_str() != "--watch").cloned().collect();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    };
    let Some(main_file) = args.get(2).cloned() else { usage(2) };
    let sources = |current: &Vec<PathBuf>| -> Vec<PathBuf> {
        match compile_file(std::path::Path::new(&main_file), None) {
            Ok(p) => p.sources.into_iter().chain(p.assets).collect(),
            Err(_) if !current.is_empty() => current.clone(),
            Err(_) => vec![PathBuf::from(&main_file)],
        }
    };
    let stamps = |paths: &[PathBuf]| -> Vec<Option<std::time::SystemTime>> {
        paths.iter().map(|p| std::fs::metadata(p).ok().and_then(|m| m.modified().ok())).collect()
    };

    let mut watched = sources(&Vec::new());
    println!("velo {VERSION} watching {} file(s)", watched.len());
    loop {
        let child = std::process::Command::new(&exe).args(&child_args).spawn();
        let mut child = match child {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("velo: {e}");
                None
            }
        };
        let mut before = stamps(&watched);
        loop {
            std::thread::sleep(Duration::from_millis(400));
            let now = stamps(&watched);
            if now != before {
                before = now;
                println!("velo: change detected, restarting");
                break;
            }
        }
        if let Some(child) = child.as_mut() {
            unsafe { kill(child.id() as i32, 15) };
            let _ = child.wait();
        }
        watched = sources(&watched);
    }
}

fn run(args: &[String]) {
    let data = flag(args, "--data").or_else(|| std::env::var("VELO_DATA").ok()).map(PathBuf::from);
    let addr = args
        .get(3)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("VELO_ADDR").ok())
        .unwrap_or_else(|| ":8080".to_string());
    let addr =
        if let Some(port) = addr.strip_prefix(':') { format!("0.0.0.0:{port}") } else { addr };
    let wal = flag(args, "--wal").or_else(|| std::env::var("VELO_WAL").ok()).map(PathBuf::from);
    let owned = data.as_ref().or(wal.as_ref());
    let _lock = owned.map(|p| {
        let mut path = p.clone().into_os_string();
        path.push(".lock");
        match velo::wal::Lock::take(std::path::Path::new(&path)) {
            Ok(lock) => lock,
            Err(e) => {
                eprintln!("velo: {e}");
                exit(1)
            }
        }
    });
    let store = Store::new();
    let prog = program(args, Some(store.clone()));
    if let Some(path) = &data {
        if let Err(e) = store.load_file(path) {
            eprintln!("velo: {}: {e}", path.display());
            exit(1)
        }
    }
    if let Some(path) = &wal {
        match store.attach_wal(path) {
            Ok(0) => {}
            Ok(n) => println!("velo: replayed {n} writes from {}", path.display()),
            Err(e) => {
                eprintln!("velo: {}: {e}", path.display());
                exit(1)
            }
        }
    }
    if let Some(path) = &data {
        let every = Duration::from_millis(
            std::env::var("VELO_SAVE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(200),
        );
        store.autosave(path.clone(), every);
    }
    let rules = velo::store::expire_rules(std::env::var("VELO_EXPIRE").ok());
    if !rules.is_empty() {
        let every = Duration::from_millis(
            std::env::var("VELO_EXPIRE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000),
        );
        store.autoexpire(rules, every.max(Duration::from_millis(100)));
    }
    let n = prog.routes.len();
    let server = match Server::new(prog) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    };
    match &data {
        Some(p) => println!("velo {VERSION} serving {n} routes on {addr}, data {}", p.display()),
        None => println!("velo {VERSION} serving {n} routes on {addr}"),
    }
    velo::http::install_signal_handlers();
    if let Err(e) = server.listen(&addr) {
        eprintln!("velo: {e}");
        exit(1)
    }
    if let Some(path) = &data {
        let mark = store.wal().map(|w| w.len()).unwrap_or(0);
        if let Err(e) = store.save_to(path) {
            eprintln!("velo: save {}: {e}", path.display());
            exit(1)
        }
        if let Some(w) = store.wal() {
            w.drop_prefix(mark);
        }
    }
    println!("velo: stopped");
}

const STARTER: &str = "GET    /health       => \"ok\"\n\
GET    /openapi.json => openapi()\n\
\n\
GET    /items        => db.items.all()\n\
GET    /items/:id    => db.items.find(id)\n\
GET    /items/search => db.items.search(\"name\", query.q)\n\
POST   /items        => db.items.create(body) when body.name else 400\n\
PUT    /items/:id    => db.items.upsert(id, body)\n\
DELETE /items/:id    => db.items.delete(id) : 204\n\
\n\
GET    /stats        => { items: db.items.count() }\n";

fn new(args: &[String]) {
    let Some(path) = args.get(2) else { usage(2) };
    if std::path::Path::new(path).exists() {
        eprintln!("velo: {path} already exists");
        exit(1)
    }
    if let Err(e) = std::fs::write(path, STARTER) {
        eprintln!("velo: {path}: {e}");
        exit(1)
    }
    println!("wrote {path}, run it with: velo run {path} :8080");
}

fn openapi(args: &[String]) {
    let path = args.get(2).cloned().unwrap_or_default();
    let title = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("velo api")
        .to_string();
    let p = program(args, None);
    let doc = velo::openapi::document(&p, &title, VERSION);
    println!("{}", String::from_utf8_lossy(&doc));
}

fn check(args: &[String]) {
    let prog = program(args, None);
    match Server::new(prog) {
        Ok(s) => println!("ok: {} routes", s.routes.len()),
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    }
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = args.iter();
    while let Some(a) = at.next() {
        if a == name {
            if let Some(v) = at.next() {
                out.push(v.clone());
            }
        }
    }
    out
}

fn sample_path(
    pattern: &str,
    col: Option<&std::sync::Arc<velo::store::Collection>>,
) -> Option<String> {
    let id = col?.sample_id()?;
    if !url_safe(&id) {
        return None;
    }
    let filled: Vec<String> = pattern
        .split('/')
        .map(|seg| if seg.starts_with(':') { id.clone() } else { seg.to_string() })
        .collect();
    Some(filled.join("/"))
}

fn url_safe(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b))
}

fn sample_query(
    given: Option<&String>,
    fields: &[String],
    col: Option<&std::sync::Arc<velo::store::Collection>>,
) -> String {
    if let Some(q) = given {
        return format!("?{}", q.trim_start_matches('?'));
    }
    if fields.is_empty() {
        return String::new();
    }
    let Some(row) = col.and_then(|c| c.sample_row()) else { return String::new() };
    let mut out = String::new();
    for name in fields {
        let value = row.get(name).as_key();
        if !url_safe(&value) {
            continue;
        }
        out.push(if out.is_empty() { '?' } else { '&' });
        out.push_str(name);
        out.push('=');
        out.push_str(&value);
    }
    out
}

fn sample_body(
    given: Option<String>,
    r: &velo::parser::Route,
    col: Option<&std::sync::Arc<velo::store::Collection>>,
) -> Option<String> {
    if let Some(body) = given {
        return Some(body);
    }
    if !r.uses_body {
        return Some(String::new());
    }
    let row = col?.sample_row()?;
    let mut named: Vec<&str> = Vec::new();
    let mut out = Vec::with_capacity(128);
    out.push(b'{');
    if let velo::Value::Obj(fields) | velo::Value::Row(fields, _) = &row {
        for (k, v) in fields.iter().filter(|(k, _)| &**k != "id") {
            if out.len() > 1 {
                out.push(b',');
            }
            named.push(k);
            velo::value::write_string(&mut out, k);
            out.push(b':');
            v.write_json(&mut out);
        }
    }
    for want in r.body_fields.iter().filter(|f| !named.contains(&f.as_str())) {
        if out.len() > 1 {
            out.push(b',');
        }
        velo::value::write_string(&mut out, want);
        out.extend_from_slice(b":\"x\"");
    }
    out.push(b'}');
    (out.len() > 2).then(|| String::from_utf8_lossy(&out).into_owned())
}

fn bench(args: &[String]) {
    let conns: usize = flag(args, "-c").and_then(|v| v.parse().ok()).unwrap_or(8);
    let secs: u64 = flag(args, "-d").and_then(|v| v.parse().ok()).unwrap_or(2);
    let store = Store::new();
    let prog = program(args, Some(store.clone()));
    if let Some(path) = flag(args, "--data") {
        if let Err(e) = store.load_file(std::path::Path::new(&path)) {
            eprintln!("velo: {path}: {e}");
            exit(1)
        }
    }
    let loaded: usize = store.names().iter().map(|n| store.collection(n).count()).sum();
    let headers: Vec<String> = flags(args, "-H");
    let given = flag(args, "-b");
    let query = flag(args, "-q");
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    let mut skipped = Vec::new();
    for r in &prog.routes {
        let method = r.method.name().to_string();
        let col = velo::ast::first_collection(&r.expr);
        let path = match r.params.len() {
            0 => Some(r.pattern.clone()),
            1 => sample_path(&r.pattern, col),
            _ => None,
        };
        let why = match (path, method.as_str()) {
            (None, _) if r.params.len() > 1 => "needs more than one path parameter",
            (None, _) => "no row to take a path parameter from",
            (_, "DELETE") => "would delete the rows it is measuring",
            (_, "HEAD" | "OPTIONS") => "answers no body of its own",
            (Some(path), "GET") => {
                let q = sample_query(query.as_ref(), &r.query_fields, col);
                reads.push((method, format!("{path}{q}"), String::new()));
                continue;
            }
            (Some(path), _) => match sample_body(given.clone(), r, col) {
                Some(body) => {
                    let q = sample_query(query.as_ref(), &r.query_fields, col);
                    writes.push((method, format!("{path}{q}"), body));
                    continue;
                }
                None => "no row to build a body from, pass -b",
            },
        };
        skipped.push((method, r.pattern.clone(), why));
    }
    let writing = !writes.is_empty();
    let mut targets = reads;
    targets.append(&mut writes);
    if targets.is_empty() {
        eprintln!("velo: nothing to bench, every route needs a parameter, a guard or a body");
        exit(1)
    }
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let server = match Server::new(prog) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    };
    let bg = server.clone();
    std::thread::spawn(move || bg.serve(velo::socket::Listener::Tcp(listener)));
    println!(
        "velo {VERSION} benching {} route(s), {conns} conns, {secs}s each, {loaded} row(s) loaded",
        targets.len()
    );
    if loaded == 0 {
        println!("note: the store is empty, pass --data to bench against real rows");
    }
    if writing {
        println!("note: write routes run last and change the store they write to");
    }
    let mut rows = Vec::new();
    for (method, path, body) in &targets {
        let r = velo::bench::run(velo::bench::Args {
            port,
            path: path.clone(),
            method: method.clone(),
            body: body.clone(),
            conns,
            secs,
            headers: headers.clone(),
            ..Default::default()
        });
        rows.push((format!("{method} {path}"), r));
    }
    server.shutdown();
    rows.sort_by(|a, b| a.1.per_second().partial_cmp(&b.1.per_second()).unwrap_or(Ordering::Equal));
    let width = rows.iter().map(|(l, _)| l.len()).max().unwrap_or(10);
    for (label, r) in &rows {
        println!(
            "{label:width$}  {:>9.0} req/s  p50 {:>7.3} ms  p99 {:>7.3} ms  {:>7.1} MB/s{}",
            r.per_second(),
            r.p50,
            r.p99,
            r.bytes as f64 / r.elapsed / 1e6,
            match (r.errors, r.refused) {
                (0, 0) => String::new(),
                (0, n) => format!("  {n} refused"),
                (e, 0) => format!("  {e} errors"),
                (e, n) => format!("  {e} errors, {n} refused"),
            }
        );
    }
    for (method, path, why) in &skipped {
        println!("{:width$}  skipped, {why}", format!("{method} {path}"));
    }
}

fn routes(args: &[String]) {
    for r in &program(args, None).routes {
        let kind = match (&r.konst, r.uses_body, r.uses_query) {
            (Some(_), _, _) => "const",
            (None, true, _) => "body",
            (None, _, true) => "query",
            _ => "dynamic",
        };
        let guard = match &r.guard {
            Some(_) => format!("guard {}", r.guard_status),
            None => String::new(),
        };
        let origin = match &r.source {
            Some(file) => format!("{file}:{}", r.line),
            None => String::new(),
        };
        println!(
            "{:<7} {:<26} {:<8} {:<4} {:<10} {}",
            r.method.name(),
            r.pattern,
            kind,
            r.status,
            guard,
            origin
        );
    }
}
