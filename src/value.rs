use std::fmt::Write as _;
use std::sync::Arc;

pub type Obj = Vec<(Arc<str>, Value)>;

#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(Arc<str>),
    Arr(Arc<Vec<Value>>),
    Obj(Arc<Obj>),
    Row(Arc<Obj>, Arc<[u8]>),
    Raw(Arc<Vec<u8>>),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Arc::from(s))
    }

    pub fn obj(fields: Obj) -> Value {
        Value::Obj(Arc::new(fields))
    }

    pub fn object(fields: &[(&str, Value)]) -> Value {
        Value::obj(fields.iter().map(|(k, v)| (intern(k), v.clone())).collect())
    }

    pub fn row(fields: Obj) -> Value {
        let obj = Arc::new(fields);
        let mut json = Vec::with_capacity(64);
        Value::Obj(obj.clone()).write_json(&mut json);
        Value::Row(obj, Arc::from(json.as_slice()))
    }

    pub fn get(&self, key: &str) -> Value {
        match self {
            Value::Obj(o) | Value::Row(o, _) => {
                o.iter().find(|(k, _)| &**k == key).map(|(_, v)| v.clone()).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    }

    pub fn get_ref(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(o) | Value::Row(o, _) => o.iter().find(|(k, _)| &**k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_at(&self, key: &str, at: &mut usize) -> Option<&Value> {
        let (Value::Obj(o) | Value::Row(o, _)) = self else { return None };
        if let Some((k, v)) = o.get(*at) {
            if &**k == key {
                return Some(v);
            }
        }
        match o.iter().position(|(k, _)| &**k == key) {
            Some(found) => {
                *at = found;
                Some(&o[found].1)
            }
            None => walk_path(self, key),
        }
    }

    pub fn key_eq(&self, want: &str) -> bool {
        match self {
            Value::Str(s) => &**s == want,
            Value::Null => want.is_empty(),
            Value::Bool(true) => want == "true",
            Value::Bool(false) => want == "false",
            Value::Num(n) => {
                let mut buf = [0u8; 32];
                let mut out = Vec::new();
                let _ = &mut buf;
                write_number(&mut out, *n);
                out == want.as_bytes()
            }
            other => other.as_key() == want,
        }
    }

    pub fn as_key_arc(&self) -> Arc<str> {
        match self {
            Value::Str(s) => s.clone(),
            other => Arc::from(other.as_key().as_str()),
        }
    }

    pub fn as_key_ref(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Value::Str(s) => std::borrow::Cow::Borrowed(s),
            Value::Null => std::borrow::Cow::Borrowed(""),
            other => std::borrow::Cow::Owned(other.as_key()),
        }
    }

    pub fn as_key(&self) -> String {
        match self {
            Value::Str(s) => s.to_string(),
            Value::Num(n) => {
                let mut b = Vec::new();
                write_number(&mut b, *n);
                unsafe { String::from_utf8_unchecked(b) }
            }
            Value::Null => String::new(),
            other => {
                let mut b = Vec::new();
                other.write_json(&mut b);
                unsafe { String::from_utf8_unchecked(b) }
            }
        }
    }

    pub fn write_json(&self, out: &mut Vec<u8>) {
        match self {
            Value::Null => out.extend_from_slice(b"null"),
            Value::Bool(true) => out.extend_from_slice(b"true"),
            Value::Bool(false) => out.extend_from_slice(b"false"),
            Value::Num(n) => write_number(out, *n),
            Value::Str(s) => write_string(out, s),
            Value::Arr(items) => {
                out.push(b'[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    v.write_json(out);
                }
                out.push(b']');
            }
            Value::Row(_, json) => out.extend_from_slice(json),
            Value::Raw(json) => out.extend_from_slice(json),
            Value::Obj(fields) => {
                out.push(b'{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    write_string(out, k);
                    out.push(b':');
                    v.write_json(out);
                }
                out.push(b'}');
            }
        }
    }

    pub fn to_json(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(64);
        self.write_json(&mut b);
        b
    }
}

pub fn write_number_into(out: &mut String, n: f64) {
    let mut bytes = Vec::with_capacity(24);
    write_number(&mut bytes, n);
    out.push_str(std::str::from_utf8(&bytes).unwrap_or("0"));
}

pub fn write_number(out: &mut Vec<u8>, f: f64) {
    if !f.is_finite() {
        out.extend_from_slice(b"null");
        return;
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        write_i64(out, f as i64);
        return;
    }
    let _ = write!(OutFmt(out), "{}", f);
}

pub fn write_i64(out: &mut Vec<u8>, n: i64) {
    let mut buf = [0u8; 20];
    let neg = n < 0;
    let mut u = if neg { (n as i128).unsigned_abs() } else { n as u128 };
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (u % 10) as u8;
        u /= 10;
        if u == 0 {
            break;
        }
    }
    if neg {
        out.push(b'-');
    }
    out.extend_from_slice(&buf[i..]);
}

struct OutFmt<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for OutFmt<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

pub fn write_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for i in 0..bytes.len() {
        let c = bytes[i];
        if c >= 0x20 && c != b'"' && c != b'\\' {
            continue;
        }
        if start < i {
            out.extend_from_slice(&bytes[start..i]);
        }
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            _ => {
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(c >> 4) as usize]);
                out.push(HEX[(c & 0xf) as usize]);
            }
        }
        start = i + 1;
    }
    if start < bytes.len() {
        out.extend_from_slice(&bytes[start..]);
    }
    out.push(b'"');
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonError;

type Interner = std::cell::RefCell<
    std::collections::HashMap<
        Box<str>,
        Arc<str>,
        std::hash::BuildHasherDefault<crate::router::Fnv>,
    >,
>;

thread_local! {
    static KEYS: Interner = std::cell::RefCell::new(std::collections::HashMap::default());
}

const INTERN_MAX: usize = 512;

pub fn intern(name: &str) -> Arc<str> {
    if name.len() > 64 {
        return Arc::from(name);
    }
    KEYS.with(|keys| {
        let mut keys = keys.borrow_mut();
        if let Some(hit) = keys.get(name) {
            return hit.clone();
        }
        if keys.len() >= INTERN_MAX {
            keys.clear();
        }
        let shared: Arc<str> = Arc::from(name);
        keys.insert(Box::from(name), shared.clone());
        shared
    })
}

pub fn parse_json(b: &[u8]) -> Result<Value, JsonError> {
    let mut p = P { b, i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(JsonError);
    }
    Ok(v)
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() {
            match self.b[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => return,
            }
        }
    }

    fn value(&mut self) -> Result<Value, JsonError> {
        if self.i >= self.b.len() {
            return Err(JsonError);
        }
        match self.b[self.i] {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Value::Str(self.string()?)),
            b't' => self.lit(b"true").map(|_| Value::Bool(true)),
            b'f' => self.lit(b"false").map(|_| Value::Bool(false)),
            b'n' => self.lit(b"null").map(|_| Value::Null),
            _ => self.number(),
        }
    }

    fn lit(&mut self, s: &[u8]) -> Result<(), JsonError> {
        if self.b.len() < self.i + s.len() || &self.b[self.i..self.i + s.len()] != s {
            return Err(JsonError);
        }
        self.i += s.len();
        Ok(())
    }

    fn object(&mut self) -> Result<Value, JsonError> {
        self.i += 1;
        let mut fields: Obj = Vec::new();
        self.ws();
        if self.i < self.b.len() && self.b[self.i] == b'}' {
            self.i += 1;
            return Ok(Value::obj(fields));
        }
        loop {
            self.ws();
            let k = self.key()?;
            self.ws();
            if self.i >= self.b.len() || self.b[self.i] != b':' {
                return Err(JsonError);
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            match fields.iter_mut().find(|(seen, _)| *seen == k) {
                Some(slot) => slot.1 = v,
                None => fields.push((k, v)),
            }
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::obj(fields));
                }
                _ => return Err(JsonError),
            }
        }
    }

    fn array(&mut self) -> Result<Value, JsonError> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.i < self.b.len() && self.b[self.i] == b']' {
            self.i += 1;
            return Ok(Value::Arr(Arc::new(items)));
        }
        loop {
            self.ws();
            items.push(self.value()?);
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Arr(Arc::new(items)));
                }
                _ => return Err(JsonError),
            }
        }
    }

    fn key(&mut self) -> Result<Arc<str>, JsonError> {
        if self.i < self.b.len() && self.b[self.i] == b'"' {
            let start = self.i + 1;
            if let Some(end) = self.b[start..].iter().position(|&c| c == b'"' || c == b'\\') {
                if self.b[start + end] == b'"' {
                    let name =
                        std::str::from_utf8(&self.b[start..start + end]).map_err(|_| JsonError)?;
                    self.i = start + end + 1;
                    return Ok(intern(name));
                }
            }
        }
        self.string()
    }

    fn string(&mut self) -> Result<Arc<str>, JsonError> {
        if self.i >= self.b.len() || self.b[self.i] != b'"' {
            return Err(JsonError);
        }
        self.i += 1;
        let start = self.i;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'"' => {
                    let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| JsonError)?;
                    self.i += 1;
                    return Ok(Arc::from(s));
                }
                b'\\' => return self.string_slow(start),
                _ => self.i += 1,
            }
        }
        Err(JsonError)
    }

    fn string_slow(&mut self, start: usize) -> Result<Arc<str>, JsonError> {
        let mut buf = Vec::with_capacity(self.b.len() - start);
        buf.extend_from_slice(&self.b[start..self.i]);
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'"' => {
                    self.i += 1;
                    let s = String::from_utf8(buf).map_err(|_| JsonError)?;
                    return Ok(Arc::from(s.as_str()));
                }
                b'\\' => {
                    self.i += 1;
                    let e = *self.b.get(self.i).ok_or(JsonError)?;
                    match e {
                        b'"' => buf.push(b'"'),
                        b'\\' => buf.push(b'\\'),
                        b'/' => buf.push(b'/'),
                        b'n' => buf.push(b'\n'),
                        b't' => buf.push(b'\t'),
                        b'r' => buf.push(b'\r'),
                        b'b' => buf.push(0x08),
                        b'f' => buf.push(0x0c),
                        b'u' => {
                            let cp = self.hex4()?;
                            let ch = if (0xD800..0xDC00).contains(&cp) {
                                let saved = self.i;
                                if self.b.get(self.i + 1) == Some(&b'\\')
                                    && self.b.get(self.i + 2) == Some(&b'u')
                                {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                        char::from_u32(c).unwrap_or('\u{fffd}')
                                    } else {
                                        self.i = saved;
                                        '\u{fffd}'
                                    }
                                } else {
                                    '\u{fffd}'
                                }
                            } else {
                                char::from_u32(cp).unwrap_or('\u{fffd}')
                            };
                            let mut tmp = [0u8; 4];
                            buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                        }
                        _ => return Err(JsonError),
                    }
                    self.i += 1;
                }
                c => {
                    buf.push(c);
                    self.i += 1;
                }
            }
        }
        Err(JsonError)
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        if self.i + 4 >= self.b.len() {
            return Err(JsonError);
        }
        let s = std::str::from_utf8(&self.b[self.i + 1..self.i + 5]).map_err(|_| JsonError)?;
        let n = u32::from_str_radix(s, 16).map_err(|_| JsonError)?;
        self.i += 4;
        Ok(n)
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.i;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E' => self.i += 1,
                _ => break,
            }
        }
        if start == self.i {
            return Err(JsonError);
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| JsonError)?;
        s.parse::<f64>().map(Value::Num).map_err(|_| JsonError)
    }
}

pub struct Keep {
    name: Arc<str>,
    children: Option<Vec<Keep>>,
}

pub fn keep_plan(names: &[Arc<str>]) -> Vec<Keep> {
    let mut plan = Vec::with_capacity(names.len());
    for name in names {
        if name.contains('.') {
            add_path(&mut plan, name);
            continue;
        }
        match plan.iter().position(|k: &Keep| k.name == *name) {
            Some(at) => plan[at].children = None,
            None => plan.push(Keep { name: name.clone(), children: None }),
        }
    }
    plan
}

pub fn any_nested(names: &[Arc<str>]) -> bool {
    names.iter().any(|n| n.contains('.'))
}

fn add_path(into: &mut Vec<Keep>, path: &str) {
    match path.split_once('.') {
        None => {
            let at = slot(into, path, true);
            into[at].children = None;
        }
        Some((head, rest)) => {
            let at = slot(into, head, false);
            if let Some(kids) = into[at].children.as_mut() {
                add_path(kids, rest);
            }
        }
    }
}

fn slot(into: &mut Vec<Keep>, name: &str, leaf: bool) -> usize {
    if let Some(at) = into.iter().position(|k| &*k.name == name) {
        return at;
    }
    let children = if leaf { None } else { Some(Vec::new()) };
    into.push(Keep { name: intern(name), children });
    into.len() - 1
}

#[cold]
fn walk_path<'a>(row: &'a Value, field: &str) -> Option<&'a Value> {
    let (head, mut rest) = field.split_once('.')?;
    let mut at = row.get_ref(head)?;
    while let Some((next, tail)) = rest.split_once('.') {
        at = at.get_ref(next)?;
        rest = tail;
    }
    at.get_ref(rest)
}

pub fn keep_selected(v: &Value, plan: &[Keep]) -> Value {
    match v {
        Value::Arr(items) => {
            Value::Arr(Arc::new(items.iter().map(|i| keep_object_planned(i, plan)).collect()))
        }
        other => keep_object_planned(other, plan),
    }
}

pub fn keep_planned(v: &Value, plan: &[Keep]) -> Value {
    match v {
        Value::Arr(items) => {
            Value::Arr(Arc::new(items.iter().map(|i| keep_planned(i, plan)).collect()))
        }
        other => keep_object_planned(other, plan),
    }
}

pub fn keep_object_planned(v: &Value, plan: &[Keep]) -> Value {
    match v {
        Value::Obj(_) | Value::Row(_, _) => {
            let mut kept: Obj = Vec::with_capacity(plan.len());
            for keep in plan {
                let Some(found) = v.get_ref(&keep.name) else { continue };
                match &keep.children {
                    None => kept.push((keep.name.clone(), found.clone())),
                    Some(kids) => {
                        let narrowed = keep_planned(found, kids);
                        if !matches!(narrowed, Value::Null) {
                            kept.push((keep.name.clone(), narrowed));
                        }
                    }
                }
            }
            Value::obj(kept)
        }
        _ => Value::Null,
    }
}
