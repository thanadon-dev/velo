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

pub fn run(src: &str, addr: &str) -> Result<(), String> {
    let prog = compile(src, None)?;
    let server = Server::new(prog)?;
    server.listen(addr).map_err(|e| e.to_string())
}
