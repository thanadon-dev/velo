use crate::store::{Agg, Collection};
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
pub const CONFLICT: Err_ = Err_ { status: 409, msg: "already exists" };

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
    Bin(BinOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Builtin {
    Openapi,
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
    Search(Box<Expr>, Box<Expr>),
    Aggregate(Agg, Box<Expr>),
    First(Box<Expr>, Box<Expr>),
    Order(Box<Expr>),
    Page(Box<Expr>, Box<Expr>),
    Create(Box<Expr>),
    Update(Box<Expr>, Box<Expr>),
    Upsert(Box<Expr>, Box<Expr>),
    Delete(Box<Expr>),
    Clear,
    DeleteWhere(Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn renders_json(&self) -> bool {
        matches!(self, Expr::Object(_) | Expr::Array(_))
    }

    pub fn write_json(&self, c: &Ctx, out: &mut Vec<u8>) -> Result<(), Err_> {
        match self {
            Expr::Const(v) => v.write_json(out),
            Expr::Object(fields) => {
                out.push(b'{');
                for (i, (k, e)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    crate::value::write_string(out, k);
                    out.push(b':');
                    e.write_json(c, out)?;
                }
                out.push(b'}');
            }
            Expr::Array(items) => {
                out.push(b'[');
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    e.write_json(c, out)?;
                }
                out.push(b']');
            }
            Expr::Param(i) => {
                let raw = c.param(*i);
                if raw.contains('%') || raw.contains('+') {
                    crate::value::write_string(out, &percent_decode(raw));
                } else {
                    crate::value::write_string(out, raw);
                }
            }
            other => other.eval(c)?.write_json(out),
        }
        Ok(())
    }

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
            Expr::Bin(op, l, r) => Ok(apply(*op, &l.eval(c)?, &r.eval(c)?)),
            Expr::And(l, r) => {
                if !truthy(&l.eval(c)?) {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(truthy(&r.eval(c)?)))
            }
            Expr::Or(l, r) => {
                if truthy(&l.eval(c)?) {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(truthy(&r.eval(c)?)))
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
                Op::First(f, v) => {
                    let field = f.eval(c)?.as_key();
                    let want = v.eval(c)?.as_key();
                    col.first(&field, &want).ok_or(NOT_FOUND)
                }
                Op::Aggregate(agg, f) => Ok(col.aggregate(*agg, &f.eval(c)?.as_key())),
                Op::Search(f, v) => {
                    let field = f.eval(c)?.as_key();
                    let needle = v.eval(c)?.as_key();
                    Ok(col.search(&field, &needle))
                }
                Op::Where(f, v) => {
                    let field = f.eval(c)?.as_key();
                    let want = v.eval(c)?.as_key();
                    Ok(col.filter(&field, &want))
                }
                Op::Create(v) => match v.eval(c)? {
                    Value::Null => Err(BAD_BODY),
                    val => col.create(val).ok_or(CONFLICT),
                },
                Op::Update(k, v) => {
                    let key = match fast_key(k, c) {
                        Some(raw) => raw.to_string(),
                        None => k.eval(c)?.as_key(),
                    };
                    let patch = v.eval(c)?;
                    col.update(&key, patch).ok_or(NOT_FOUND)
                }
                Op::Upsert(k, v) => {
                    let key = match fast_key(k, c) {
                        Some(raw) => raw.to_string(),
                        None => k.eval(c)?.as_key(),
                    };
                    match v.eval(c)? {
                        Value::Null => Err(BAD_BODY),
                        val => Ok(col.upsert(&key, val)),
                    }
                }
                Op::Clear => Ok(deleted(col.clear())),
                Op::DeleteWhere(f, v) => {
                    let field = f.eval(c)?.as_key();
                    let want = v.eval(c)?.as_key();
                    Ok(deleted(col.delete_where(&field, &want)))
                }
                Op::Delete(k) => {
                    let hit = match fast_key(k, c) {
                        Some(raw) => col.delete(raw),
                        None => col.delete(&k.eval(c)?.as_key()),
                    };
                    if hit {
                        Ok(Value::obj(vec![(crate::value::intern("deleted"), Value::Bool(true))]))
                    } else {
                        Err(NOT_FOUND)
                    }
                }
            },
        }
    }
}

fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(*n),
        Value::Str(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(*b as u8 as f64),
        Value::Raw(bytes) => std::str::from_utf8(bytes).ok().and_then(|t| t.trim().parse().ok()),
        _ => None,
    }
}

pub fn apply(op: BinOp, l: &Value, r: &Value) -> Value {
    if op == BinOp::Add {
        if let (Value::Str(a), Value::Str(b)) = (l, r) {
            return Value::Str(Arc::from(format!("{a}{b}").as_str()));
        }
    }
    let (Some(a), Some(b)) = (as_num(l), as_num(r)) else {
        return match (op, l, r) {
            (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div, _, _) => Value::Null,
            (_, Value::Str(x), Value::Str(y)) => Value::Bool(match op {
                BinOp::Lt => x < y,
                BinOp::Gt => x > y,
                BinOp::Le => x <= y,
                _ => x >= y,
            }),
            _ => Value::Bool(false),
        };
    };
    match op {
        BinOp::Add => Value::Num(a + b),
        BinOp::Sub => Value::Num(a - b),
        BinOp::Mul => Value::Num(a * b),
        BinOp::Div => {
            if b == 0.0 {
                Value::Null
            } else {
                Value::Num(a / b)
            }
        }
        BinOp::Lt => Value::Bool(a < b),
        BinOp::Gt => Value::Bool(a > b),
        BinOp::Le => Value::Bool(a <= b),
        BinOp::Ge => Value::Bool(a >= b),
    }
}

fn deleted(n: usize) -> Value {
    Value::obj(vec![(crate::value::intern("deleted"), Value::Num(n as f64))])
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
        Builtin::Openapi => Value::Null,
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
        static STATE: Cell<u64> = const { Cell::new(0) };
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
        fields.push((
            std::sync::Arc::from(key.as_str()),
            Value::Str(std::sync::Arc::from(val.as_str())),
        ));
    }
    Value::obj(fields)
}
