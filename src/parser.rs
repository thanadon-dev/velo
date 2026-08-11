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
}

impl<'a> Default for Ctx<'a> {
    fn default() -> Self {
        Ctx { params: [""; MAX_PARAMS], nparams: 0, body: Value::Null }
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
    Field(Box<Expr>, Arc<str>),
    Object(Vec<(Arc<str>, Expr)>),
    Array(Vec<Expr>),
    Db(Arc<Collection>, Op),
}

pub enum Op {
    All,
    Count,
    Find(Box<Expr>),
    Create(Box<Expr>),
    Update(Box<Expr>, Box<Expr>),
    Delete(Box<Expr>),
}

impl Expr {
    pub fn eval(&self, c: &Ctx) -> Result<Value, Err_> {
        match self {
            Expr::Const(v) => Ok(v.clone()),
            Expr::Param(i) => Ok(Value::str(c.param(*i))),
            Expr::Body => Ok(c.body.clone()),
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
            Expr::Db(col, op) => match op {
                Op::All => Ok(col.all()),
                Op::Count => Ok(Value::Num(col.count() as f64)),
                Op::Find(k) => col.find(&k.eval(c)?.as_key()).ok_or(NOT_FOUND),
                Op::Create(v) => match v.eval(c)? {
                    Value::Null => Err(BAD_BODY),
                    val => Ok(col.create(val)),
                },
                Op::Update(k, v) => {
                    let key = k.eval(c)?.as_key();
                    let patch = v.eval(c)?;
                    col.update(&key, patch).ok_or(NOT_FOUND)
                }
                Op::Delete(k) => {
                    if col.delete(&k.eval(c)?.as_key()) {
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
    };
    p.advance()?;
    let mut routes = Vec::new();
    while p.tok.kind != Kind::Eof {
        routes.push(p.route()?);
    }
    if routes.is_empty() {
        return Err("no routes defined".to_string());
    }
    Ok(Program { routes, store })
}

struct Parser<'a> {
    lex: Lexer<'a>,
    tok: Token,
    store: Arc<Store>,
    params: Vec<String>,
    pure: bool,
    body: bool,
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
        let expr = self.expr()?;
        let status = if method == Method::Post { 201 } else { 200 };
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
            _ => {}
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
