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
         \x20 velo run <file.velo> [addr] [--data file.json] [--watch]\n\
         \x20                               start the server (default :8080, env VELO_ADDR)\n\
         \x20 velo check <file.velo>        compile only, report errors\n\
         \x20 velo routes <file.velo>       list compiled routes\n\
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
            Ok(p) => p.sources,
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
    let store = Store::new();
    let prog = program(args, Some(store.clone()));
    if let Some(path) = &data {
        if let Err(e) = store.load_file(path) {
            eprintln!("velo: {}: {e}", path.display());
            exit(1)
        }
        let every = Duration::from_millis(
            std::env::var("VELO_SAVE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(200),
        );
        store.autosave(path.clone(), every);
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
        if let Err(e) = store.save_to(path) {
            eprintln!("velo: save {}: {e}", path.display());
            exit(1)
        }
    }
    println!("velo: stopped");
}

const STARTER: &str = "GET    /health      => \"ok\"\n\
GET    /items       => db.items.all()\n\
GET    /items/:id   => db.items.find(id)\n\
POST   /items       => db.items.create(body)\n\
PUT    /items/:id   => db.items.update(id, body)\n\
DELETE /items/:id   => db.items.delete(id) : 204\n";

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
        println!("{:<7} {:<28} {:<8} {:<4} {}", r.method.name(), r.pattern, kind, r.status, guard);
    }
}
