#[cfg(not(target_os = "linux"))]
compile_error!("velo's event loop is built on epoll, so it only builds on Linux");

pub mod ast;
pub mod date;
pub mod epoll;
pub mod http;
pub mod lexer;
pub mod openapi;
pub mod parser;
pub mod router;
pub mod serve;
pub mod store;
pub mod value;

pub use http::{Ctype, Server};
pub use parser::{compile, Method, Program, Route};
pub use store::Store;
pub use value::Value;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn compile_file(
    path: &std::path::Path,
    store: Option<std::sync::Arc<Store>>,
) -> Result<Program, String> {
    let store = store.unwrap_or_default();
    let mut seen = Vec::new();
    let mut prog = Program {
        routes: Vec::new(),
        store: store.clone(),
        includes: Vec::new(),
        sources: Vec::new(),
    };
    load_into(&mut prog, path, &store, &mut seen)?;
    if prog.routes.is_empty() {
        return Err("no routes defined".to_string());
    }
    parser::bake_openapi(&mut prog);
    Ok(prog)
}

fn load_into(
    prog: &mut Program,
    path: &std::path::Path,
    store: &std::sync::Arc<Store>,
    seen: &mut Vec<std::path::PathBuf>,
) -> Result<(), String> {
    let full = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if seen.contains(&full) {
        return Ok(());
    }
    seen.push(full.clone());
    prog.sources.push(full.clone());
    if seen.len() > 32 {
        return Err(format!("{}: include nesting too deep", path.display()));
    }
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let part =
        compile(&src, Some(store.clone())).map_err(|e| format!("{}: {e}", path.display()))?;
    prog.routes.extend(part.routes);
    let dir = full.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for include in part.includes {
        load_into(prog, &dir.join(include), store, seen)?;
    }
    Ok(())
}

pub fn run(src: &str, addr: &str) -> Result<(), String> {
    let prog = compile(src, None)?;
    let server = Server::new(prog)?;
    server.listen(addr).map_err(|e| e.to_string())
}
