use crate::lexer::{Kind, Lexer, Token};
use crate::store::{Collection, Store};
use crate::value::Value;
use std::sync::Arc;

pub const MAX_PARAMS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Err_ {
    pub status: u16,
    pub msg: &'static str,
}

pub const NOT_FOUND: Err_ = Err_ { status: 404, msg: "not found" };
pub const BAD_BODY: Err_ = Err_ { status: 400, msg: "invalid body" };

pub struct Ctx<'a> {
    pub params: [&'a str; MAX_PARAMS],
    pub nparams: usize,
    pub body: Value,
    pub query: Value,
    pub header: Value,
}

impl<'a> Default for Ctx<'a> {
    fn default() -> Self {
        Ctx {
            params: [""; MAX_PARAMS],
            nparams: 0,
            body: Value::Null,
            query: Value::Null,
            header: Value::Null,
        }
    }
}

impl<'a> Ctx<'a> {
    pub fn param(&self, i: usize) -> &'a str {
        if i < self.nparams {
            self.params[i]
        } else {
            ""
        }
    }
}

pub enum Expr {
    Const(Value),
    Param(usize),
    Body,
    Query,
    Header,
    Field(Box<Expr>, Arc<str>),
    Object(Vec<(Arc<str>, Expr)>),
    Array(Vec<Expr>),
    Db(Arc<Collection>, Op),
    Call(Builtin, Vec<Expr>),
    Cmp(Box<Expr>, bool, Box<Expr>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Builtin {
    Now,
    Uuid,
    Len,
    Env,
}

pub enum Op {
    All,
    Count,
    Find(Box<Expr>),
    Where(Box<Expr>, Box<Expr>),
    Order(Box<Expr>),
    Page(Box<Expr>, Box<Expr>),
    Create(Box<Expr>),
    Update(Box<Expr>, Box<Expr>),
    Delete(Box<Expr>),
}

impl Expr {
    pub fn eval(&self, c: &Ctx) -> Result<Value, Err_> {
        match self {
            Expr::Const(v) => Ok(v.clone()),
            Expr::Param(i) => Ok(decode_param(c.param(*i))),
            Expr::Body => Ok(c.body.clone()),
            Expr::Query => Ok(c.query.clone()),
            Expr::Header => Ok(c.header.clone()),
            Expr::Field(base, key) => Ok(base.eval(c)?.get(key)),
            Expr::Object(fields) => {
                let mut out = Vec::with_capacity(fields.len());
                for (k, e) in fields {
                    out.push((k.clone(), e.eval(c)?));
                }
                Ok(Value::obj(out))
            }
            Expr::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for e in items {
                    out.push(e.eval(c)?);
                }
                Ok(Value::Arr(Arc::new(out)))
            }
            Expr::Cmp(l, eq, r) => {
                let same = l.eval(c)?.as_key() == r.eval(c)?.as_key();
                Ok(Value::Bool(same == *eq))
            }
            Expr::Call(f, args) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(a.eval(c)?);
                }
                Ok(call_builtin(*f, &vals))
            }
            Expr::Db(col, op) => match op {
                Op::All => Ok(col.all()),
                Op::Count => Ok(Value::Num(col.count() as f64)),
                Op::Find(k) => match fast_key(k, c) {
                    Some(raw) => col.find(raw).ok_or(NOT_FOUND),
                    None => col.find(&k.eval(c)?.as_key()).ok_or(NOT_FOUND),
                },
                Op::Page(o, l) => {
                    let offset = num_arg(&o.eval(c)?);
                    let limit = num_arg(&l.eval(c)?);
                    Ok(col.page(offset, limit))
                }
                Op::Order(f) => Ok(col.order(&f.eval(c)?.as_key())),
                Op::Where(f, v) => {
                    let field = f.eval(c)?.as_key();
                    let want = v.eval(c)?.as_key();
                    Ok(col.filter(&field, &want))
                }
                Op::Create(v) => match v.eval(c)? {
                    Value::Null => Err(BAD_BODY),
                    val => Ok(col.create(val)),
                },
                Op::Update(k, v) => {
                    let key = match fast_key(k, c) {
                        Some(raw) => raw.to_string(),
                        None => k.eval(c)?.as_key(),
                    };
                    let patch = v.eval(c)?;
                    col.update(&key, patch).ok_or(NOT_FOUND)
                }
                Op::Delete(k) => {
                    let hit = match fast_key(k, c) {
                        Some(raw) => col.delete(raw),
                        None => col.delete(&k.eval(c)?.as_key()),
                    };
                    if hit {
                        Ok(Value::obj(vec![(Arc::from("deleted"), Value::Bool(true))]))
                    } else {
                        Err(NOT_FOUND)
                    }
                }
            },
        }
    }
}

pub struct Route {
    pub method: Method,
    pub pattern: String,
    pub params: Vec<String>,
    pub expr: Expr,
    pub konst: Option<Vec<u8>>,
    pub const_text: bool,
    pub status: u16,
    pub uses_body: bool,
    pub uses_query: bool,
    pub uses_header: bool,
    pub guard: Option<Expr>,
    pub line: usize,
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
    pub fn from_str(s: &str) -> Option<Method> {
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
}

pub fn compile(src: &str, store: Option<Arc<Store>>) -> Result<Program, String> {
    let store = store.unwrap_or_else(Store::new);
    let mut p = Parser {
        lex: Lexer::new(src),
        tok: Token { kind: Kind::Eof, text: String::new(), num: 0.0, line: 0 },
        store: store.clone(),
        params: Vec::new(),
        pure: true,
        body: false,
        query: false,
        header: false,
    };
    p.advance().map_err(|e| with_source(src, e))?;
    let mut routes = Vec::new();
    while p.tok.kind != Kind::Eof {
        routes.push(p.route().map_err(|e| with_source(src, e))?);
    }
    if routes.is_empty() {
        return Err("no routes defined".to_string());
    }
    Ok(Program { routes, store })
}

fn with_source(src: &str, err: String) -> String {
    let Some(rest) = err.strip_prefix("line ") else { return err };
    let Some(num) = rest.split(':').next().and_then(|n| n.parse::<usize>().ok()) else {
        return err;
    };
    let Some(text) = src.lines().nth(num.saturating_sub(1)) else { return err };
    let gutter = num.to_string();
    format!(
        "{err}\n  {gutter} | {}\n  {} |",
        text.trim_end(),
        " ".repeat(gutter.len())
    )
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
}

impl<'a> Parser<'a> {
    fn advance(&mut self) -> Result<(), String> {
        self.tok = self.lex.next()?;
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
        let method = Method::from_str(&self.tok.text.to_uppercase())
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
            let left = self.expr()?;
            let g = match self.tok.kind {
                Kind::Eq | Kind::Ne => {
                    let eq = self.tok.kind == Kind::Eq;
                    self.advance()?;
                    Expr::Cmp(Box::new(left), eq, Box::new(self.expr()?))
                }
                _ => left,
            };
            self.pure = false;
            Some(g)
        } else {
            None
        };
        let (konst, const_text) = if self.pure {
            match expr.eval(&Ctx::default()) {
                Ok(Value::Str(s)) => (Some(s.as_bytes().to_vec()), true),
                Ok(v) => (Some(v.to_json()), false),
                Err(_) => (None, false),
            }
        } else {
            (None, false)
        };
        Ok(Route {
            method,
            pattern: path.text,
            params,
            expr,
            konst,
            const_text,
            status,
            uses_body: self.body,
            uses_query: self.query,
            uses_header: self.header,
            guard,
            line,
        })
    }

    fn expr(&mut self) -> Result<Expr, String> {
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
            _ => Err(format!(
                "line {}: unexpected {:?} in expression",
                self.tok.line, self.tok.text
            )),
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
                return self.fields(Expr::Query);
            }
            "header" => {
                self.pure = false;
                self.header = true;
                return self.fields(Expr::Header);
            }
            _ => {}
        }
        if self.tok.kind == Kind::LParen {
            let f = match head.text.as_str() {
                "now" => Builtin::Now,
                "uuid" => Builtin::Uuid,
                "len" => Builtin::Len,
                "env" => Builtin::Env,
                other => {
                    return Err(format!("line {}: unknown function {other}()", head.line))
                }
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
                Builtin::Now | Builtin::Uuid => 0,
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
        Err(format!(
            "line {}: unknown identifier {:?}",
            head.line, head.text
        ))
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
                return Err(format!(
                    "line {}: unknown operation db.{}.{}",
                    line, name.text, other
                ))
            }
        };
        Ok(Expr::Db(col, op))
    }
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0,
        Value::Str(s) => !s.is_empty(),
        _ => true,
    }
}

fn fast_key<'a>(e: &Expr, c: &Ctx<'a>) -> Option<&'a str> {
    let Expr::Param(i) = e else { return None };
    let raw = c.param(*i);
    if raw.contains('%') || raw.contains('+') {
        return None;
    }
    Some(raw)
}

pub fn call_builtin(f: Builtin, args: &[Value]) -> Value {
    match f {
        Builtin::Now => Value::Num(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as f64)
                .unwrap_or(0.0),
        ),
        Builtin::Uuid => Value::Str(Arc::from(uuid_v4().as_str())),
        Builtin::Len => Value::Num(match &args[0] {
            Value::Str(s) => s.chars().count() as f64,
            Value::Arr(a) => a.len() as f64,
            Value::Obj(o) | Value::Row(o, _) => o.len() as f64,
            Value::Null => 0.0,
            _ => 1.0,
        }),
        Builtin::Env => match std::env::var(args[0].as_key()) {
            Ok(v) => Value::Str(Arc::from(v.as_str())),
            Err(_) => Value::Null,
        },
    }
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    fill_random(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = b"0123456789abcdef";
    let mut out = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push(hex[(byte >> 4) as usize] as char);
        out.push(hex[(byte & 0xf) as usize] as char);
    }
    out
}

fn fill_random(out: &mut [u8]) {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = Cell::new(0);
    }
    STATE.with(|st| {
        let mut x = st.get();
        if x == 0 {
            let mut seed = [0u8; 8];
            if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
                use std::io::Read;
                let _ = f.read_exact(&mut seed);
            }
            x = u64::from_ne_bytes(seed)
                ^ std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(1);
            if x == 0 {
                x = 0x9e3779b97f4a7c15;
            }
        }
        for chunk in out.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let bytes = x.to_ne_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        st.set(x);
    });
}

fn num_arg(v: &Value) -> usize {
    match v {
        Value::Num(n) if *n >= 0.0 => *n as usize,
        Value::Str(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn decode_param(s: &str) -> Value {
    if !s.contains('%') && !s.contains('+') {
        return Value::str(s);
    }
    Value::Str(std::sync::Arc::from(percent_decode(s).as_str()))
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match hex(b[i + 1]).zip(hex(b[i + 2])) {
                Some((h, l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                None => {
                    out.push(b[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

pub fn parse_query(q: &str) -> Value {
    let mut fields: Vec<(std::sync::Arc<str>, Value)> = Vec::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = percent_decode(k);
        let val = percent_decode(v);
        fields.push((std::sync::Arc::from(key.as_str()), Value::Str(std::sync::Arc::from(val.as_str()))));
    }
    Value::obj(fields)
}

fn pattern_params(pattern: &str, line: usize) -> Result<Vec<String>, String> {
    let mut params = Vec::new();
    for seg in pattern.trim_matches('/').split('/') {
        if let Some(name) = seg.strip_prefix(':') {
            if name.is_empty() {
                return Err(format!(
                    "line {}: empty parameter name in {:?}",
                    line, pattern
                ));
            }
            params.push(name.to_string());
        }
    }
    if params.len() > MAX_PARAMS {
        return Err(format!(
            "line {}: too many parameters (max {})",
            line, MAX_PARAMS
        ));
    }
    Ok(params)
}
