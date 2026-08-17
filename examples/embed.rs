use std::path::Path;
use velo::socket::Listener;
use velo::{compile_file, Server, Store, Value};

fn main() -> Result<(), String> {
    let store = Store::new();
    let program = compile_file(Path::new("examples/todo.velo"), Some(store.clone()))?;

    let todos = store.collection("todos");
    for text in ["read the readme", "run the benchmark", "ship something"] {
        todos.create(
            Value::object(&[("text", Value::str(text)), ("done", Value::Bool(false))]),
            &[],
        );
    }

    let server = Server::new(program)?;
    println!("seeded {} todos, {} routes", todos.count(), server.routes.len());

    let mut out = Vec::new();
    let (status, ctype) = server.dispatch("GET", "/todos", b"", &mut out);
    println!("GET /todos -> {status} {ctype}");
    println!("{}", String::from_utf8_lossy(&out));

    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:8080".to_string());
    if std::env::var("VELO_EMBED_SERVE").is_ok() {
        println!("serving on {addr}");
        server
            .serve(Listener::bind(&addr).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
