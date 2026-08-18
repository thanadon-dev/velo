use crate::store::{Agg, Cmp, Collection};
use crate::value::Value;
use std::cell::RefCell;
use std::sync::Arc;

pub const MAX_PARAMS: usize = 8;
pub const MAX_LOCALS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Err_ {
    pub status: u16,
    pub msg: &'static str,
}

pub const NOT_FOUND: Err_ = Err_ { status: 404, msg: "not found" };
pub const BAD_BODY: Err_ = Err_ { status: 400, msg: "invalid body" };
pub const CONFLICT: Err_ = Err_ { status: 409, msg: "already exists" };
pub const TOO_MANY: Err_ = Err_ { status: 429, msg: "too many requests" };
pub const NOT_NUMBER: Err_ = Err_ { status: 409, msg: "not a number" };

pub struct Ctx<'a> {
    pub params: [&'a str; MAX_PARAMS],
    pub nparams: usize,
    pub body: Value,
    pub query: Value,
    pub query_raw: &'a str,
    pub header: Value,
    pub header_raw: &'a [u8],
    pub cookies: RefCell<Vec<u8>>,
    pub locals: RefCell<[Value; MAX_LOCALS]>,
    pub reason: RefCell<Option<Arc<str>>>,
}

impl<'a> Default for Ctx<'a> {
    fn default() -> Self {
        Ctx {
            params: [""; MAX_PARAMS],
            nparams: 0,
            body: Value::Null,
            query: Value::Null,
            query_raw: "",
            header: Value::Null,
            header_raw: &[],
            cookies: RefCell::new(Vec::new()),
            locals: RefCell::new(std::array::from_fn(|_| Value::Null)),
            reason: RefCell::new(None),
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
    QueryField(Arc<str>),
    Header,
    HeaderField(Arc<str>),
    CookieField(Arc<str>),
    Field(Box<Expr>, Arc<str>),
    Select(Box<Expr>, Vec<Expr>),
    With(Box<Expr>, Arc<str>, Arc<Collection>, Arc<str>),
    Object(Vec<(Arc<str>, Expr)>),
    Array(Vec<Expr>),
    Db(Arc<Collection>, Op),
    Call(Builtin, Vec<Expr>),
    Cmp(Box<Expr>, bool, Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    Let(usize, Box<Expr>, Box<Expr>),
    Do(Vec<Expr>),
    Local(usize),
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
    Date,
    Uuid,
    Len,
    Env,
    Default,
    Lower,
    Upper,
    Trim,
    Hash,
    Password,
    Verify,
    SetCookie,
    Limit,
    Post,
    Check,
}

pub enum Stage {
    Where(Box<Expr>, Cmp, Box<Expr>),
    Search(Box<Expr>, Box<Expr>),
    Order(Box<Expr>),
    Page(Box<Expr>, Box<Expr>),
}

pub enum Tail {
    List,
    Rows,
    Count,
    Agg(Agg, Box<Expr>),
    First(Vec<Expr>),
    Select(Vec<Expr>),
    Group(Box<Expr>, Option<(Agg, Box<Expr>)>),
}

pub enum Op {
    All,
    Count,
    Find(Box<Expr>, Vec<Expr>),
    Where(Box<Expr>, Box<Expr>),
    Search(Box<Expr>, Box<Expr>),
    Aggregate(Agg, Box<Expr>),
    First(Box<Expr>, Box<Expr>, Vec<Expr>),
    Order(Box<Expr>),
    Page(Box<Expr>, Box<Expr>),
    Create(Box<Expr>, Vec<Expr>),
    Update(Box<Expr>, Box<Expr>, Vec<Expr>),
    Incr(Box<Expr>, Box<Expr>, Box<Expr>),
    Upsert(Box<Expr>, Box<Expr>, Vec<Expr>),
    Delete(Box<Expr>),
    Clear,
    DeleteWhere(Box<Expr>, Cmp, Box<Expr>),
    Chain(Vec<Stage>, Tail),
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
            Expr::QueryField(name) => Ok(query_value(c.query_raw, name)),
            Expr::Header => Ok(c.header.clone()),
            Expr::HeaderField(name) => Ok(crate::http::header_value(c.header_raw, name)),
            Expr::CookieField(name) => Ok(crate::http::cookie_value(c.header_raw, name)),
            Expr::Field(base, key) => Ok(base.eval(c)?.get(key)),
            Expr::With(base, key, other, out) => {
                Ok(crate::store::attach(&base.eval(c)?, key, other, out))
            }
            Expr::Do(steps) => {
                let mut plan = Vec::with_capacity(steps.len());
                for step in steps {
                    let Expr::Db(col, op) = step else { return Err(BAD_BODY) };
                    plan.push((col.clone(), tx_step(op, c)?));
                }
                match crate::store::commit(plan) {
                    Ok(rows) => Ok(Value::Arr(Arc::new(rows))),
                    Err((_, why)) => Err(match why {
                        crate::store::Refused::Conflict => CONFLICT,
                        crate::store::Refused::Missing => NOT_FOUND,
                        crate::store::Refused::NotNumber => NOT_NUMBER,
                    }),
                }
            }
            Expr::Let(slot, value, body) => {
                let bound = value.eval(c)?;
                c.locals.borrow_mut()[*slot] = bound;
                body.eval(c)
            }
            Expr::Local(slot) => Ok(c.locals.borrow()[*slot].clone()),
            Expr::If(cond, yes, no) => match truthy(&cond.eval(c)?) {
                true => yes.eval(c),
                false => no.eval(c),
            },
            Expr::Select(base, fields) => {
                let mut names = Vec::with_capacity(fields.len());
                for f in fields {
                    names.push(f.eval(c)?.as_key_arc());
                }
                Ok(keep_fields(base.eval(c)?, &names))
            }
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
                if *f == Builtin::Check {
                    if truthy(&vals[0]) {
                        return Ok(Value::Bool(true));
                    }
                    *c.reason.borrow_mut() = Some(vals[1].as_key_arc());
                    return Err(BAD_BODY);
                }
                if *f == Builtin::Limit {
                    let rate = match as_num(&vals[1]) {
                        Some(rate) if rate >= 1.0 => rate as u32,
                        _ => return Err(TOO_MANY),
                    };
                    if !crate::http::within_limit(&vals[0].as_key(), rate) {
                        return Err(TOO_MANY);
                    }
                    return Ok(Value::Bool(true));
                }
                if *f == Builtin::Post {
                    let value = vals.pop().unwrap_or(Value::Null);
                    let url = vals.pop().unwrap_or(Value::Null);
                    crate::hook::send(&url.as_key(), value.to_json());
                    return Ok(value);
                }
                if *f == Builtin::SetCookie {
                    let value = vals.pop().unwrap_or(Value::Null);
                    let name = vals.pop().unwrap_or(Value::Null);
                    crate::http::set_cookie(
                        &mut c.cookies.borrow_mut(),
                        &name.as_key(),
                        &value.as_key(),
                    );
                    return Ok(value);
                }
                Ok(call_builtin(*f, &vals))
            }
            Expr::Db(col, op) => match op {
                Op::All => Ok(col.all()),
                Op::Count => Ok(Value::Num(col.count() as f64)),
                Op::Find(k, fields) => {
                    let row = match fast_key(k, c) {
                        Some(raw) => col.find(raw),
                        None => col.find(&k.eval(c)?.as_key_ref()),
                    };
                    project(row.ok_or(NOT_FOUND)?, fields, c)
                }
                Op::Page(o, l) => {
                    let offset = num_arg(&o.eval(c)?);
                    let limit = num_arg(&l.eval(c)?);
                    Ok(col.page(offset, limit))
                }
                Op::Order(f) => Ok(col.order(&f.eval(c)?.as_key_ref())),
                Op::First(f, v, fields) => {
                    let field = f.eval(c)?;
                    let want = v.eval(c)?;
                    let row =
                        col.first(&field.as_key_ref(), &want.as_key_ref()).ok_or(NOT_FOUND)?;
                    project(row, fields, c)
                }
                Op::Aggregate(agg, f) => Ok(col.aggregate(*agg, &f.eval(c)?.as_key_ref())),
                Op::Search(f, v) => {
                    let field = f.eval(c)?;
                    let needle = v.eval(c)?;
                    Ok(col.search(&field.as_key_ref(), &needle.as_key_ref()))
                }
                Op::Where(f, v) => {
                    let field = f.eval(c)?;
                    let want = v.eval(c)?;
                    Ok(col.filter(&field.as_key_ref(), &want.as_key_ref()))
                }
                Op::Create(v, unique) => match v.eval(c)? {
                    Value::Null => Err(BAD_BODY),
                    val => {
                        let names = field_names(unique, c)?;
                        let fields: Vec<&str> = names.iter().map(|n| &**n).collect();
                        col.create(val, &fields).ok_or(CONFLICT)
                    }
                },
                Op::Update(k, v, unique) => {
                    let key = match fast_key(k, c) {
                        Some(raw) => raw.to_string(),
                        None => k.eval(c)?.as_key(),
                    };
                    let patch = v.eval(c)?;
                    let names = field_names(unique, c)?;
                    let fields: Vec<&str> = names.iter().map(|n| &**n).collect();
                    match col.update(&key, patch, &fields) {
                        Some(row) => Ok(row),
                        None if fields.is_empty() => Err(NOT_FOUND),
                        None if col.has(&key) => Err(CONFLICT),
                        None => Err(NOT_FOUND),
                    }
                }
                Op::Incr(k, f, by) => {
                    let key = match fast_key(k, c) {
                        Some(raw) => raw.to_string(),
                        None => k.eval(c)?.as_key(),
                    };
                    let field = f.eval(c)?;
                    let Some(by) = as_num(&by.eval(c)?) else {
                        return Err(BAD_BODY);
                    };
                    match col.incr(&key, &field.as_key_ref(), by) {
                        crate::store::Incr::Done(row) => Ok(row),
                        crate::store::Incr::Missing => Err(NOT_FOUND),
                        crate::store::Incr::NotNumber => Err(NOT_NUMBER),
                    }
                }
                Op::Upsert(k, v, unique) => {
                    let key = match fast_key(k, c) {
                        Some(raw) => raw.to_string(),
                        None => k.eval(c)?.as_key(),
                    };
                    match v.eval(c)? {
                        Value::Null => Err(BAD_BODY),
                        val => {
                            let names = field_names(unique, c)?;
                            let fields: Vec<&str> = names.iter().map(|n| &**n).collect();
                            col.upsert(&key, val, &fields).ok_or(CONFLICT)
                        }
                    }
                }
                Op::Clear => Ok(deleted(col.clear())),
                Op::DeleteWhere(f, op, v) => {
                    let field = f.eval(c)?;
                    let want = v.eval(c)?;
                    Ok(deleted(col.delete_where(&field.as_key_ref(), *op, &list_arg(want, *op))))
                }
                Op::Chain(stages, tail) => {
                    let mut plan = Vec::with_capacity(stages.len());
                    for s in stages {
                        plan.push(match s {
                            Stage::Where(f, op, v) => crate::store::Stage::Where(
                                f.eval(c)?.as_key_arc(),
                                *op,
                                list_arg(v.eval(c)?, *op),
                            ),
                            Stage::Search(f, v) => crate::store::Stage::Search(
                                f.eval(c)?.as_key_arc(),
                                v.eval(c)?.as_key_arc(),
                            ),
                            Stage::Order(f) => crate::store::Stage::Order(f.eval(c)?.as_key_arc()),
                            Stage::Page(o, l) => crate::store::Stage::Page(
                                num_arg(&o.eval(c)?),
                                num_arg(&l.eval(c)?),
                            ),
                        });
                    }
                    match tail {
                        Tail::Group(by, agg) => {
                            let by = by.eval(c)?.as_key_arc();
                            let op = match agg {
                                Some((a, f)) => Some((*a, f.eval(c)?.as_key_arc())),
                                None => None,
                            };
                            Ok(col.query_group(&plan, &by, op))
                        }
                        Tail::List => Ok(col.query(&plan)),
                        Tail::Rows => Ok(col.query_rows(&plan)),
                        Tail::Count => Ok(col.query_count(&plan)),
                        Tail::Agg(agg, f) => Ok(col.query_agg(&plan, *agg, &f.eval(c)?.as_key())),
                        Tail::First(fields) => {
                            let row = col.query_first(&plan).ok_or(NOT_FOUND)?;
                            Ok(project(row, fields, c)?)
                        }
                        Tail::Select(fields) => {
                            let mut names = Vec::with_capacity(fields.len());
                            for f in fields {
                                names.push(f.eval(c)?.as_key_arc());
                            }
                            Ok(col.query_select(&plan, &names))
                        }
                    }
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

fn list_arg(v: Value, op: Cmp) -> Arc<str> {
    match v {
        Value::Arr(items) if op == Cmp::In => {
            let mut out = String::new();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&item.as_key_ref());
            }
            out.into()
        }
        other => other.as_key_arc(),
    }
}

fn tx_key(k: &Expr, c: &Ctx) -> Result<String, Err_> {
    Ok(match fast_key(k, c) {
        Some(raw) => raw.to_string(),
        None => k.eval(c)?.as_key(),
    })
}

fn tx_step(op: &Op, c: &Ctx) -> Result<crate::store::Step, Err_> {
    Ok(match op {
        Op::Create(v, unique) => match v.eval(c)? {
            Value::Null => return Err(BAD_BODY),
            val => crate::store::Step::Create(val, field_names(unique, c)?),
        },
        Op::Update(k, v, unique) => {
            crate::store::Step::Update(tx_key(k, c)?, v.eval(c)?, field_names(unique, c)?)
        }
        Op::Upsert(k, v, unique) => match v.eval(c)? {
            Value::Null => return Err(BAD_BODY),
            val => crate::store::Step::Upsert(tx_key(k, c)?, val, field_names(unique, c)?),
        },
        Op::Incr(k, f, by) => {
            let key = tx_key(k, c)?;
            let field = f.eval(c)?.as_key_arc();
            let Some(by) = as_num(&by.eval(c)?) else { return Err(BAD_BODY) };
            crate::store::Step::Incr(key, field, by)
        }
        Op::Delete(k) => crate::store::Step::Delete(tx_key(k, c)?),
        _ => return Err(BAD_BODY),
    })
}

fn field_names(unique: &[Expr], c: &Ctx) -> Result<Vec<Arc<str>>, Err_> {
    let mut names = Vec::with_capacity(unique.len());
    for f in unique {
        names.push(f.eval(c)?.as_key_arc());
    }
    Ok(names)
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

fn project(row: Value, fields: &[Expr], c: &Ctx) -> Result<Value, Err_> {
    if fields.is_empty() {
        return Ok(row);
    }
    let mut names = Vec::with_capacity(fields.len());
    for f in fields {
        names.push(f.eval(c)?.as_key_arc());
    }
    Ok(keep_fields(row, &names))
}

fn keep_fields(v: Value, names: &[Arc<str>]) -> Value {
    crate::value::keep_selected(&v, &crate::value::keep_plan(names))
}

pub fn first_collection(e: &Expr) -> Option<&Arc<Collection>> {
    match e {
        Expr::Db(col, _) => Some(col),
        Expr::Field(base, _) => first_collection(base),
        Expr::Select(base, fields) => {
            first_collection(base).or_else(|| fields.iter().find_map(first_collection))
        }
        Expr::With(base, ..) => first_collection(base),
        Expr::Let(_, value, body) => first_collection(value).or_else(|| first_collection(body)),
        Expr::If(cond, yes, no) => first_collection(cond)
            .or_else(|| first_collection(yes))
            .or_else(|| first_collection(no)),
        Expr::Object(fields) => fields.iter().find_map(|(_, e)| first_collection(e)),
        Expr::Array(items) | Expr::Call(_, items) => items.iter().find_map(first_collection),
        Expr::Cmp(l, _, r) | Expr::And(l, r) | Expr::Or(l, r) | Expr::Bin(_, l, r) => {
            first_collection(l).or_else(|| first_collection(r))
        }
        _ => None,
    }
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Raw(json) => truthy_json(json),
        _ => true,
    }
}

fn truthy_json(json: &[u8]) -> bool {
    match json.first() {
        None => false,
        Some(b'n') => false,
        Some(b'f') => false,
        Some(b'"') => json.len() > 2,
        Some(c) if c.is_ascii_digit() || *c == b'-' => std::str::from_utf8(json)
            .ok()
            .and_then(|t| t.parse::<f64>().ok())
            .map(|n| n != 0.0)
            .unwrap_or(true),
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
        Builtin::Date => match as_num(&args[0]) {
            Some(ms) if ms >= 0.0 => Value::Str(Arc::from(crate::date::iso(ms as u64).as_str())),
            _ => Value::Null,
        },
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
        Builtin::Default => match &args[0] {
            Value::Null => args[1].clone(),
            Value::Str(s) if s.is_empty() => args[1].clone(),
            given => given.clone(),
        },
        Builtin::Lower => text_of(&args[0], |t| t.to_lowercase()),
        Builtin::Upper => text_of(&args[0], |t| t.to_uppercase()),
        Builtin::Trim => text_of(&args[0], |t| t.trim().to_string()),
        Builtin::Hash => Value::Str(Arc::from(
            crate::crypto::hex(&crate::crypto::sha256(args[0].as_key().as_bytes())).as_str(),
        )),
        Builtin::Password => {
            Value::Str(Arc::from(crate::crypto::password(&args[0].as_key()).as_str()))
        }
        Builtin::SetCookie | Builtin::Post => args[1].clone(),
        Builtin::Limit => Value::Bool(true),
        Builtin::Check => Value::Bool(true),
        Builtin::Verify => Value::Bool(match &args[1] {
            Value::Str(stored) => crate::crypto::verify(&args[0].as_key(), stored),
            _ => false,
        }),
    }
}

fn text_of(v: &Value, f: impl FnOnce(&str) -> String) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Str(s) => Value::Str(Arc::from(f(s).as_str())),
        other => Value::Str(Arc::from(f(&other.as_key()).as_str())),
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
    crate::crypto::random(out);
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

pub fn query_value(raw: &str, name: &str) -> Value {
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, val) = pair.split_once('=').unwrap_or((pair, ""));
        let hit = if key.contains('%') || key.contains('+') {
            percent_decode(key) == name
        } else {
            key == name
        };
        if hit {
            return decode_param(val);
        }
    }
    Value::Null
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
