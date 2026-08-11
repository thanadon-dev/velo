use std::process::exit;
use velo::{compile, Server, VERSION};

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
         \x20 velo run <file.velo> [addr]   start the server (default :8080, env VELO_ADDR)\n\
         \x20 velo check <file.velo>        compile only, report errors\n\
         \x20 velo routes <file.velo>       list compiled routes\n\
         \x20 velo version                  print version"
    );
    exit(code)
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
    let addr = args
        .get(3)
        .cloned()
        .or_else(|| std::env::var("VELO_ADDR").ok())
        .unwrap_or_else(|| ":8080".to_string());
    let addr = if let Some(port) = addr.strip_prefix(':') {
        format!("0.0.0.0:{port}")
    } else {
        addr
    };
    let prog = match compile(&source, None) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    };
    let n = prog.routes.len();
    let server = match Server::new(prog) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("velo: {e}");
            exit(1)
        }
    };
    println!("velo {VERSION} serving {n} routes on {addr}");
    if let Err(e) = server.listen(&addr) {
        eprintln!("velo: {e}");
        exit(1)
    }
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
                let kind = match (&r.konst, r.uses_body) {
                    (Some(_), _) => "const",
                    (None, true) => "body",
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
