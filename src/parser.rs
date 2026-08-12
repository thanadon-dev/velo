use crate::lexer::{Kind, Lexer, Token};
use crate::store::Store;
use crate::value::Value;
use std::sync::Arc;

pub use crate::ast::{
    call_builtin, decode_param, parse_query, percent_decode, truthy, BinOp, Builtin, Ctx, Err_,
    Expr, Op, BAD_BODY, CONFLICT, MAX_PARAMS, NOT_FOUND,
};

pub struct Route {
    pub method: Method,
    pub pattern: String,
    pub params: Vec<String>,
    pub expr: Expr,
    pub konst: Option<Vec<u8>>,
    pub const_ctype: crate::http::Ctype,
    pub status: u16,
    pub uses_body: bool,
    pub uses_query: bool,
    pub uses_header: bool,
    pub query_fields: Vec<String>,
    pub header_fields: Vec<String>,
    pub guard: Option<Expr>,
    pub guard_status: u16,
    pub line: usize,
    pub source: Option<Arc<str>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

pub const N_METHODS: usize = 7;

impl Method {
    pub fn parse(s: &str) -> Option<Method> {
        Some(match s {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "HEAD" => Method::Head,
            "OPTIONS" => Method::Options,
            _ => return None,
        })
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
        }
    }
}

pub struct Program {
    pub routes: Vec<Route>,
    pub store: Arc<Store>,
    pub includes: Vec<String>,
    pub sources: Vec<std::path::PathBuf>,
}

pub fn compile(src: &str, store: Option<Arc<Store>>) -> Result<Program, String> {
    compile_in(src, store, std::path::Path::new("."))
}

pub fn compile_in(
    src: &str,
    store: Option<Arc<Store>>,
    base: &std::path::Path,
) -> Result<Program, String> {
    let store = store.unwrap_or_default();
    let mut p = Parser {
        lex: Lexer::new(src),
        tok: Token { kind: Kind::Eof, text: String::new(), num: 0.0, line: 0 },
        store: store.clone(),
        params: Vec::new(),
        pure: true,
        body: false,
        query: false,
        header: false,
        query_fields: Vec::new(),
        header_fields: Vec::new(),
        base: base.to_path_buf(),
        file_ctype: None,
    };
    p.advance().map_err(|e| with_source(src, e))?;
    let mut routes = Vec::new();
    let mut includes = Vec::new();
    while p.tok.kind != Kind::Eof {
        if p.tok.kind == Kind::Ident && p.tok.text == "include" {
            p.advance().map_err(|e| with_source(src, e))?;
            let file = p.expect(Kind::Str).map_err(|e| with_source(src, e))?;
            includes.push(file.text);
            continue;
        }
        routes.push(p.route().map_err(|e| with_source(src, e))?);
    }
    if routes.is_empty() && includes.is_empty() {
        return Err("no routes defined".to_string());
    }
    let mut prog = Program { routes, store, includes, sources: Vec::new() };
    bake_openapi(&mut prog);
    Ok(prog)
}

pub fn bake_openapi(prog: &mut Program) {
    let is_doc = |r: &Route| matches!(&r.expr, Expr::Call(Builtin::Openapi, _));
    if !prog.routes.iter().any(is_doc) {
        return;
    }
    let title = std::env::var("VELO_TITLE").unwrap_or_else(|_| "velo api".to_string());
    let doc = crate::openapi::document(prog, &title, crate::VERSION);
    for r in prog.routes.iter_mut() {
        if matches!(&r.expr, Expr::Call(Builtin::Openapi, _)) {
            r.konst = Some(doc.clone());
            r.const_ctype = crate::http::JSON;
        }
    }
}

fn remember(list: &mut Vec<String>, name: &str) {
    if !list.iter().any(|n| n == name) {
        list.push(name.to_string());
    }
}

fn with_source(src: &str, err: String) -> String {
    let Some(rest) = err.strip_prefix("line ") else { return err };
    let Some(num) = rest.split(':').next().and_then(|n| n.parse::<usize>().ok()) else {
        return err;
    };
    let Some(text) = src.lines().nth(num.saturating_sub(1)) else { return err };
    let gutter = num.to_string();
    format!("{err}\n  {gutter} | {}\n  {} |", text.trim_end(), " ".repeat(gutter.len()))
}

struct Parser<'a> {
    lex: Lexer<'a>,
    tok: Token,
    store: Arc<Store>,
    params: Vec<String>,
    pure: bool,
    body: bool,
    query: bool,
    header: bool,
    query_fields: Vec<String>,
    header_fields: Vec<String>,
    base: std::path::PathBuf,
    file_ctype: Option<crate::http::Ctype>,
}

impl<'a> Parser<'a> {
    fn advance(&mut self) -> Result<(), String> {
        self.tok = self.lex.next_token()?;
        Ok(())
    }

    fn expect(&mut self, k: Kind) -> Result<Token, String> {
        if self.tok.kind != k {
            return Err(format!(
                "line {}: expected {}, got {:?}",
                self.tok.line,
                k.name(),
                self.tok.text
            ));
        }
        let t = self.tok.clone();
        self.advance()?;
        Ok(t)
    }

    fn route(&mut self) -> Result<Route, String> {
        if self.tok.kind != Kind::Ident {
            return Err(format!(
                "line {}: expected http method, got {:?}",
                self.tok.line, self.tok.text
            ));
        }
        let line = self.tok.line;
        let method = Method::parse(&self.tok.text.to_uppercase())
            .ok_or_else(|| format!("line {}: unknown method {:?}", line, self.tok.text))?;
        let path = self.lex.path()?;
        self.advance()?;
        self.expect(Kind::Arrow)?;
        let params = pattern_params(&path.text, path.line)?;
        self.params = params.clone();
        self.pure = true;
        self.body = false;
        self.query = false;
        self.header = false;
        self.query_fields.clear();
        self.header_fields.clear();
        self.file_ctype = None;
        let expr = self.expr()?;
        let mut status = if method == Method::Post { 201 } else { 200 };
        if self.tok.kind == Kind::Colon {
            self.advance()?;
            let code = self.expect(Kind::Num)?;
            if code.num < 100.0 || code.num > 599.0 || code.num.fract() != 0.0 {
                return Err(format!("line {}: bad status {}", code.line, code.text));
            }
            status = code.num as u16;
        }
        let guard = if self.tok.kind == Kind::Ident && self.tok.text == "when" {
            self.advance()?;
            let g = self.expr()?;
            self.pure = false;
            Some(g)
        } else {
            None
        };
        let mut guard_status = 401;
        if guard.is_some() && self.tok.kind == Kind::Ident && self.tok.text == "or" {
            self.advance()?;
            let code = self.expect(Kind::Num)?;
            if !(100.0..=599.0).contains(&code.num) || code.num.fract() != 0.0 {
                return Err(format!("line {}: bad status {}", code.line, code.text));
            }
            guard_status = code.num as u16;
        }
        let (konst, const_ctype) = if self.pure {
            match (&self.file_ctype, expr.eval(&Ctx::default())) {
                (Some(ct), Ok(Value::Str(s))) => (Some(s.as_bytes().to_vec()), *ct),
                (None, Ok(Value::Str(s))) => (Some(s.as_bytes().to_vec()), crate::http::TEXT),
                (_, Ok(v)) => (Some(v.to_json()), crate::http::JSON),
                (_, Err(_)) => (None, crate::http::JSON),
            }
        } else {
            (None, crate::http::JSON)
        };
        Ok(Route {
            method,
            pattern: path.text,
            params,
            expr,
            konst,
            const_ctype,
            status,
            uses_body: self.body,
            uses_query: self.query,
            uses_header: self.header,
            source: None,
            query_fields: std::mem::take(&mut self.query_fields),
            header_fields: std::mem::take(&mut self.header_fields),
            guard,
            guard_status,
            line,
        })
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let left = self.additive()?;
        let op = match self.tok.kind {
            Kind::Eq => return self.compare(left, true),
            Kind::Ne => return self.compare(left, false),
            Kind::Lt => BinOp::Lt,
            Kind::Gt => BinOp::Gt,
            Kind::Le => BinOp::Le,
            Kind::Ge => BinOp::Ge,
            _ => return Ok(left),
        };
        self.advance()?;
        let right = self.additive()?;
        Ok(Expr::Bin(op, Box::new(left), Box::new(right)))
    }

    fn compare(&mut self, left: Expr, eq: bool) -> Result<Expr, String> {
        self.advance()?;
        let right = self.additive()?;
        Ok(Expr::Cmp(Box::new(left), eq, Box::new(right)))
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.tok.kind {
                Kind::Plus => BinOp::Add,
                Kind::Minus => BinOp::Sub,
                _ => return Ok(left),
            };
            self.advance()?;
            let right = self.multiplicative()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.primary()?;
        loop {
            let op = match self.tok.kind {
                Kind::Star => BinOp::Mul,
                Kind::Slash => BinOp::Div,
                _ => return Ok(left),
            };
            self.advance()?;
            let right = self.primary()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        if self.tok.kind == Kind::LParen {
            self.advance()?;
            let inner = self.expr()?;
            self.expect(Kind::RParen)?;
            return Ok(inner);
        }
        match self.tok.kind {
            Kind::Str => {
                let e = Expr::Const(Value::str(&self.tok.text));
                self.advance()?;
                Ok(e)
            }
            Kind::Num => {
                let e = Expr::Const(Value::Num(self.tok.num));
                self.advance()?;
                Ok(e)
            }
            Kind::LBrace => self.object(),
            Kind::LBrack => self.array(),
            Kind::Ident => self.chain(),
            _ => {
                Err(format!("line {}: unexpected {:?} in expression", self.tok.line, self.tok.text))
            }
        }
    }

    fn object(&mut self) -> Result<Expr, String> {
        self.advance()?;
        let mut fields = Vec::new();
        while self.tok.kind != Kind::RBrace {
            if self.tok.kind != Kind::Ident && self.tok.kind != Kind::Str {
                return Err(format!(
                    "line {}: expected object key, got {:?}",
                    self.tok.line, self.tok.text
                ));
            }
            let key: Arc<str> = Arc::from(self.tok.text.as_str());
            self.advance()?;
            self.expect(Kind::Colon)?;
            let v = self.expr()?;
            fields.push((key, v));
            if self.tok.kind == Kind::Comma {
                self.advance()?;
            }
        }
        self.advance()?;
        Ok(Expr::Object(fields))
    }

    fn array(&mut self) -> Result<Expr, String> {
        self.advance()?;
        let mut items = Vec::new();
        while self.tok.kind != Kind::RBrack {
            items.push(self.expr()?);
            if self.tok.kind == Kind::Comma {
                self.advance()?;
            }
        }
        self.advance()?;
        Ok(Expr::Array(items))
    }

    fn chain(&mut self) -> Result<Expr, String> {
        let head = self.tok.clone();
        self.advance()?;
        match head.text.as_str() {
            "true" => return Ok(Expr::Const(Value::Bool(true))),
            "false" => return Ok(Expr::Const(Value::Bool(false))),
            "null" => return Ok(Expr::Const(Value::Null)),
            "db" => {
                self.pure = false;
                return self.db_call(head.line);
            }
            "body" => {
                self.pure = false;
                self.body = true;
                return self.fields(Expr::Body);
            }
            "query" => {
                self.pure = false;
                self.query = true;
                let e = self.fields(Expr::Query)?;
                if let Expr::Field(_, name) = &e {
                    remember(&mut self.query_fields, name);
                }
                return Ok(e);
            }
            "header" => {
                self.pure = false;
                self.header = true;
                let e = self.fields(Expr::Header)?;
                if let Expr::Field(_, name) = &e {
                    remember(&mut self.header_fields, name);
                }
                return Ok(e);
            }
            _ => {}
        }
        if self.tok.kind == Kind::LParen {
            let f = match head.text.as_str() {
                "file" => {
                    self.advance()?;
                    if self.tok.kind != Kind::Str {
                        return Err(format!("line {}: file() needs a path", head.line));
                    }
                    let rel = self.tok.text.clone();
                    self.advance()?;
                    if self.tok.kind != Kind::RParen {
                        return Err(format!("line {}: file() takes one path", head.line));
                    }
                    self.advance()?;
                    let path = self.base.join(&rel);
                    let text = std::fs::read_to_string(&path)
                        .map_err(|e| format!("line {}: {}: {e}", head.line, path.display()))?;
                    self.file_ctype = Some(crate::http::ctype_for(&rel));
                    return Ok(Expr::Const(Value::str(&text)));
                }
                "openapi" => Builtin::Openapi,
                "now" => Builtin::Now,
                "uuid" => Builtin::Uuid,
                "len" => Builtin::Len,
                "env" => Builtin::Env,
                other => return Err(format!("line {}: unknown function {other}()", head.line)),
            };
            self.advance()?;
            let mut args = Vec::new();
            while self.tok.kind != Kind::RParen {
                args.push(self.expr()?);
                if self.tok.kind == Kind::Comma {
                    self.advance()?;
                }
            }
            self.advance()?;
            let arity = match f {
                Builtin::Openapi | Builtin::Now | Builtin::Uuid => 0,
                Builtin::Len | Builtin::Env => 1,
            };
            if args.len() != arity {
                return Err(format!(
                    "line {}: {}() expects {arity} argument(s), got {}",
                    head.line,
                    head.text,
                    args.len()
                ));
            }
            if f != Builtin::Env {
                self.pure = false;
            }
            return self.fields(Expr::Call(f, args));
        }
        if let Some(i) = self.params.iter().position(|p| *p == head.text) {
            self.pure = false;
            return self.fields(Expr::Param(i));
        }
        Err(format!("line {}: unknown identifier {:?}", head.line, head.text))
    }

    fn fields(&mut self, base: Expr) -> Result<Expr, String> {
        let mut cur = base;
        while self.tok.kind == Kind::Dot {
            self.advance()?;
            let name = self.expect(Kind::Ident)?;
            cur = Expr::Field(Box::new(cur), Arc::from(name.text.as_str()));
        }
        Ok(cur)
    }

    fn db_call(&mut self, line: usize) -> Result<Expr, String> {
        self.expect(Kind::Dot)?;
        let name = self.expect(Kind::Ident)?;
        let col = self.store.collection(&name.text);
        self.expect(Kind::Dot)?;
        let op = self.expect(Kind::Ident)?;
        self.expect(Kind::LParen)?;
        let mut args = Vec::new();
        while self.tok.kind != Kind::RParen {
            args.push(self.expr()?);
            if self.tok.kind == Kind::Comma {
                self.advance()?;
            }
        }
        self.advance()?;
        let n = args.len();
        let want = |k: usize| -> Result<(), String> {
            if n != k {
                return Err(format!(
                    "line {}: db.{}.{} expects {} argument(s), got {}",
                    line, name.text, op.text, k, n
                ));
            }
            Ok(())
        };
        let mut args = args.into_iter();
        let op = match op.text.as_str() {
            "all" => {
                want(0)?;
                Op::All
            }
            "count" => {
                want(0)?;
                Op::Count
            }
            "find" => {
                want(1)?;
                Op::Find(Box::new(args.next().unwrap()))
            }
            "page" => {
                want(2)?;
                let o = Box::new(args.next().unwrap());
                Op::Page(o, Box::new(args.next().unwrap()))
            }
            "order" => {
                want(1)?;
                Op::Order(Box::new(args.next().unwrap()))
            }
            "first" => {
                want(2)?;
                let f = Box::new(args.next().unwrap());
                Op::First(f, Box::new(args.next().unwrap()))
            }
            "sum" | "avg" | "min" | "max" => {
                want(1)?;
                let agg = match op.text.as_str() {
                    "sum" => crate::store::Agg::Sum,
                    "avg" => crate::store::Agg::Avg,
                    "min" => crate::store::Agg::Min,
                    _ => crate::store::Agg::Max,
                };
                Op::Aggregate(agg, Box::new(args.next().unwrap()))
            }
            "search" => {
                want(2)?;
                let f = Box::new(args.next().unwrap());
                Op::Search(f, Box::new(args.next().unwrap()))
            }
            "where" => {
                want(2)?;
                let f = Box::new(args.next().unwrap());
                Op::Where(f, Box::new(args.next().unwrap()))
            }
            "create" => {
                want(1)?;
                Op::Create(Box::new(args.next().unwrap()))
            }
            "update" => {
                want(2)?;
                let k = Box::new(args.next().unwrap());
                Op::Update(k, Box::new(args.next().unwrap()))
            }
            "delete" => {
                want(1)?;
                Op::Delete(Box::new(args.next().unwrap()))
            }
            other => {
                return Err(format!("line {}: unknown operation db.{}.{}", line, name.text, other))
            }
        };
        Ok(Expr::Db(col, op))
    }
}

fn pattern_params(pattern: &str, line: usize) -> Result<Vec<String>, String> {
    let mut params = Vec::new();
    for seg in pattern.trim_matches('/').split('/') {
        if let Some(name) = seg.strip_prefix(':') {
            if name.is_empty() {
                return Err(format!("line {}: empty parameter name in {:?}", line, pattern));
            }
            params.push(name.to_string());
        }
    }
    if params.len() > MAX_PARAMS {
        return Err(format!("line {}: too many parameters (max {})", line, MAX_PARAMS));
    }
    Ok(params)
}
