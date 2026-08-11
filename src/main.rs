use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;
use velo::{compile, Server, Store, VERSION};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    match cmd {
        "run" => run(&args),
        "check" => check(&args),
        "routes" => routes(&args),
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
         \x20 velo run <file.velo> [addr] [--data file.json]\n\
         \x20                               start the server (default :8080, env VELO_ADDR)\n\
         \x20 velo check <file.velo>        compile only, report errors\n\
         \x20 velo routes <file.velo>       list compiled routes\n\
         \x20 velo version                  print version"
    );
    exit(code)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn src(args: &[String]) -> String {
    let Some(path) = args.get(2) else { usage(2) };
    match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("velo: {path}: {e}");
            exit(1)
        }
    }
}

fn run(args: &[String]) {
    let source = src(args);
    let data = flag(args, "--data").or_else(|| std::env::var("VELO_DATA").ok()).map(PathBuf::from);
    let addr = args
        .get(3)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("VELO_ADDR").ok())
        .unwrap_or_else(|| ":8080".to_string());
    let addr = if let Some(port) = addr.strip_prefix(':') {
        format!("0.0.0.0:{port}")
    } else {
        addr
    };
    let store = Store::new();
    let prog = match compile(&source, Some(store.clone())) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    };
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

fn check(args: &[String]) {
    let source = src(args);
    match compile(&source, None) {
        Ok(p) => println!("ok: {} routes", p.routes.len()),
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    }
}

fn routes(args: &[String]) {
    let source = src(args);
    match compile(&source, None) {
        Ok(p) => {
            for r in &p.routes {
                let kind = match (&r.konst, r.uses_body, r.uses_query) {
                    (Some(_), _, _) => "const",
                    (None, true, _) => "body",
                    (None, _, true) => "query",
                    _ => "dynamic",
                };
                println!(
                    "{:<7} {:<28} {:<8} {}",
                    r.method.name(),
                    r.pattern,
                    kind,
                    r.status
                );
            }
        }
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    }
}
