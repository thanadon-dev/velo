use crate::value::{Obj, Value};
use std::cmp::Ordering as Ordering2;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

const CACHE_MAX: usize = 32;

fn with_key<R>(prefix: &str, a: &str, b: &str, f: impl FnOnce(&str) -> R) -> R {
    with_key4("", prefix, a, b, f)
}

fn with_key4<R>(tag: &str, prefix: &str, a: &str, b: &str, f: impl FnOnce(&str) -> R) -> R {
    let mut buf = [0u8; 224];
    let need = tag.len() + prefix.len() + a.len() + b.len() + 3;
    if need > buf.len() {
        return f(&format!("{tag}\0{prefix}\0{a}\0{b}"));
    }
    let mut n = 0;
    for part in [tag, "\0", prefix, "\0", a, "\0", b] {
        buf[n..n + part.len()].copy_from_slice(part.as_bytes());
        n += part.len();
    }
    match std::str::from_utf8(&buf[..n]) {
        Ok(key) => f(key),
        Err(_) => f(&format!("{tag}\0{prefix}\0{a}\0{b}")),
    }
}

fn cache_budget() -> usize {
    std::env::var("VELO_CACHE_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(8 << 20)
}

struct Snapshot {
    rows: Arc<Vec<Value>>,
    holes: usize,
    by_id: HashMap<String, usize>,
    all_json: Option<Arc<Vec<u8>>>,
    list_used: AtomicBool,
    cache: RwLock<HashMap<String, Arc<Vec<u8>>>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

fn is_live(v: &Value) -> bool {
    !matches!(v, Value::Null)
}

impl Snapshot {
    fn live(&self) -> impl Iterator<Item = &Value> {
        self.rows.iter().filter(|r| is_live(r))
    }

    fn len(&self) -> usize {
        self.rows.len() - self.holes
    }

    fn compact(&mut self) {
        if self.holes == 0 {
            return;
        }
        let rows = Arc::make_mut(&mut self.rows);
        rows.retain(is_live);
        self.holes = 0;
        self.by_id.clear();
        for (i, row) in rows.iter().enumerate() {
            self.by_id.insert(row.get("id").as_key(), i);
        }
    }

    fn json(&self) -> Arc<Vec<u8>> {
        if let Some(json) = &self.all_json {
            return json.clone();
        }
        let mut out = Vec::with_capacity(self.len() * 64 + 2);
        out.push(b'[');
        for (n, row) in self.live().enumerate() {
            if n > 0 {
                out.push(b',');
            }
            row.write_json(&mut out);
        }
        out.push(b']');
        Arc::new(out)
    }

    fn json_cached(&mut self) -> Arc<Vec<u8>> {
        let json = self.json();
        self.all_json = Some(json.clone());
        self.list_used.store(true, Ordering::Relaxed);
        json
    }

    fn append_json(&mut self, row: &Value) {
        let Some(mut old) = self.all_json.take() else { return };
        if old.len() < 2 {
            return;
        }
        if let Some(list) = Arc::get_mut(&mut old) {
            list.pop();
            if list.len() > 1 {
                list.push(b',');
            }
            row.write_json(list);
            list.push(b']');
            self.all_json = Some(old);
            return;
        }
        let mut out = Vec::with_capacity(old.len() * 2);
        out.extend_from_slice(&old[..old.len() - 1]);
        if old.len() > 2 {
            out.push(b',');
        }
        row.write_json(&mut out);
        out.push(b']');
        self.all_json = Some(Arc::new(out));
    }

    fn invalidate(&mut self) {
        self.all_json = None;
        self.list_used.store(false, Ordering::Relaxed);
        self.cache.get_mut().unwrap().clear();
    }

    fn cached(&self, key: &str) -> Option<Arc<Vec<u8>>> {
        let hit = self.cache.read().unwrap().get(key).cloned();
        let counter = if hit.is_some() { &self.hits } else { &self.misses };
        counter.fetch_add(1, Ordering::Relaxed);
        hit
    }

    fn store_cached(&self, key: &str, json: Arc<Vec<u8>>) -> Arc<Vec<u8>> {
        let budget = cache_budget();
        if json.len() > budget {
            return json;
        }
        let mut cache = self.cache.write().unwrap();
        let used: usize = cache.values().map(|v| v.len()).sum();
        if cache.len() >= CACHE_MAX || used + json.len() > budget {
            cache.clear();
        }
        cache.insert(key.to_string(), json.clone());
        json
    }

    fn filtered_json(&self, field: &str, want: &str) -> Arc<Vec<u8>> {
        if let Some(hit) = with_key("w", field, want, |k| self.cached(k)) {
            return hit;
        }
        let mut out = Vec::with_capacity(256);
        out.push(b'[');
        for (n, row) in self.live().filter(|r| field_eq(r, field, want)).enumerate() {
            if n > 0 {
                out.push(b',');
            }
            row.write_json(&mut out);
        }
        out.push(b']');
        with_key("w", field, want, |k| self.store_cached(k, Arc::new(out)))
    }

    fn searched_json(&self, field: &str, needle: &str) -> Arc<Vec<u8>> {
        if let Some(hit) = with_key("s", field, needle, |k| self.cached(k)) {
            return hit;
        }
        let lower = needle.to_lowercase();
        let mut out = Vec::with_capacity(256);
        out.push(b'[');
        for (n, row) in self.live().filter(|r| field_has(r, field, &lower)).enumerate() {
            if n > 0 {
                out.push(b',');
            }
            row.write_json(&mut out);
        }
        out.push(b']');
        with_key("s", field, needle, |k| self.store_cached(k, Arc::new(out)))
    }

    fn aggregate_json(&self, op: Agg, field: &str) -> Arc<Vec<u8>> {
        if let Some(hit) = with_key("a", op.name(), field, |k| self.cached(k)) {
            return hit;
        }
        let mut acc: Option<f64> = None;
        let mut n = 0u64;
        for row in self.live() {
            let Some(Value::Num(v)) = row.get_ref(field) else { continue };
            n += 1;
            acc = Some(match (acc, op) {
                (None, _) => *v,
                (Some(a), Agg::Sum | Agg::Avg) => a + v,
                (Some(a), Agg::Min) => a.min(*v),
                (Some(a), Agg::Max) => a.max(*v),
            });
        }
        let mut out = Vec::with_capacity(24);
        match (acc, op) {
            (Some(a), Agg::Avg) => crate::value::write_number(&mut out, a / n as f64),
            (Some(a), _) => crate::value::write_number(&mut out, a),
            (None, Agg::Sum) => out.extend_from_slice(b"0"),
            (None, _) => out.extend_from_slice(b"null"),
        }
        with_key("a", op.name(), field, |k| self.store_cached(k, Arc::new(out)))
    }

    fn sorted_json(&self, field: &str) -> Arc<Vec<u8>> {
        if let Some(hit) = with_key("o", field, "", |k| self.cached(k)) {
            return hit;
        }
        let (sort_field, desc) = match field.strip_prefix('-') {
            Some(f) => (f, true),
            None => (field, false),
        };
        let mut keyed: Vec<(SortKey, &Value)> =
            self.live().map(|r| (sort_key(r.get_ref(sort_field)), r)).collect();
        keyed.sort_by(|(a, _), (b, _)| {
            let ord = match (a, b) {
                (SortKey::Num(m), SortKey::Num(n)) => m.partial_cmp(n).unwrap_or(Ordering2::Equal),
                (SortKey::Num(_), _) => Ordering2::Less,
                (_, SortKey::Num(_)) => Ordering2::Greater,
                (SortKey::Text(m), SortKey::Text(n)) => m.cmp(n),
            };
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
        let mut out = Vec::with_capacity(self.len() * 64 + 2);
        out.push(b'[');
        for (i, (_, row)) in keyed.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            row.write_json(&mut out);
        }
        out.push(b']');
        with_key("o", field, "", |k| self.store_cached(k, Arc::new(out)))
    }
}

static NEXT_COLLECTION_ID: AtomicU64 = AtomicU64::new(0);

type LocalCache = std::cell::RefCell<HashMap<String, (u64, Arc<Vec<u8>>)>>;

thread_local! {
    static LOCAL_CACHE: LocalCache = std::cell::RefCell::new(HashMap::new());
}

const LOCAL_CACHE_MAX: usize = 64;

fn local_budget() -> usize {
    std::env::var("VELO_LOCAL_CACHE_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(1 << 20)
}

pub struct Collection {
    pub name: String,
    id: u64,
    version: AtomicU64,
    snap: RwLock<Snapshot>,
    next_id: AtomicU64,
    dirty: Arc<AtomicBool>,
}

impl Collection {
    fn new(name: &str, dirty: Arc<AtomicBool>) -> Collection {
        Collection {
            name: name.to_string(),
            id: NEXT_COLLECTION_ID.fetch_add(1, Ordering::Relaxed),
            version: AtomicU64::new(0),
            snap: RwLock::new(Snapshot {
                rows: Arc::new(Vec::new()),
                holes: 0,
                by_id: HashMap::new(),
                all_json: None,
                list_used: AtomicBool::new(false),
                cache: RwLock::new(HashMap::new()),
                hits: AtomicU64::new(0),
                misses: AtomicU64::new(0),
            }),
            next_id: AtomicU64::new(0),
            dirty,
        }
    }

    fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn bump(&self) {
        self.version.fetch_add(1, Ordering::Release);
    }

    fn tag(&self) -> String {
        let mut out = String::with_capacity(8);
        let mut v = self.id;
        loop {
            out.push((b'a' + (v % 26) as u8) as char);
            v /= 26;
            if v == 0 {
                return out;
            }
        }
    }

    fn derived(
        &self,
        kind: &str,
        a: &str,
        b: &str,
        build: impl FnOnce(&Snapshot) -> Arc<Vec<u8>>,
    ) -> Value {
        let version = self.version.load(Ordering::Acquire);
        let tag = self.tag();
        let json = with_key4(&tag, kind, a, b, |key| {
            let hit = LOCAL_CACHE.with(|c| {
                c.borrow().get(key).and_then(|(v, json)| (*v == version).then(|| json.clone()))
            });
            if let Some(json) = hit {
                return json;
            }
            let json = build(&self.snap.read().unwrap());
            LOCAL_CACHE.with(|c| {
                let mut c = c.borrow_mut();
                let used: usize = c.values().map(|(_, v)| v.len()).sum();
                if c.len() >= LOCAL_CACHE_MAX || used + json.len() > local_budget() {
                    c.clear();
                }
                if json.len() <= local_budget() {
                    c.insert(key.to_string(), (version, json.clone()));
                }
            });
            json
        });
        Value::Raw(json)
    }

    pub fn rows(&self) -> Arc<Vec<Value>> {
        let mut s = self.snap.write().unwrap();
        s.compact();
        s.rows.clone()
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed)
    }

    pub fn load(&self, rows: Vec<Value>, next_id: u64) {
        let mut s = self.snap.write().unwrap();
        s.by_id.clear();
        for (i, r) in rows.iter().enumerate() {
            s.by_id.insert(r.get("id").as_key(), i);
        }
        s.rows = Arc::new(rows.into_iter().map(as_row).collect());
        s.holes = 0;
        s.invalidate();
        self.bump();
        self.next_id.store(next_id, Ordering::Relaxed);
    }

    pub fn all(&self) -> Value {
        {
            let s = self.snap.read().unwrap();
            if let Some(json) = s.all_json.clone() {
                s.list_used.store(true, Ordering::Relaxed);
                return Value::Raw(json);
            }
        }
        Value::Raw(self.snap.write().unwrap().json_cached())
    }

    pub fn count(&self) -> usize {
        self.snap.read().unwrap().len()
    }

    pub fn find(&self, id: &str) -> Option<Value> {
        let s = self.snap.read().unwrap();
        s.by_id.get(id).map(|&i| s.rows[i].clone())
    }

    pub fn filter(&self, field: &str, want: &str) -> Value {
        self.derived("w", field, want, |s| s.filtered_json(field, want))
    }

    pub fn first(&self, field: &str, want: &str) -> Option<Value> {
        let s = self.snap.read().unwrap();
        let found = s.live().find(|r| field_eq(r, field, want)).cloned();
        found
    }

    pub fn page(&self, offset: usize, limit: usize) -> Value {
        let s = self.snap.read().unwrap();
        let take = if limit == 0 { usize::MAX } else { limit };
        let rows: Vec<Value> = s.live().skip(offset).take(take).cloned().collect();
        Value::Arr(Arc::new(rows))
    }

    pub fn cache_stats(&self) -> (u64, u64) {
        let s = self.snap.read().unwrap();
        (s.hits.load(Ordering::Relaxed), s.misses.load(Ordering::Relaxed))
    }

    pub fn aggregate(&self, op: Agg, field: &str) -> Value {
        self.derived("a", op.name(), field, |s| s.aggregate_json(op, field))
    }

    pub fn search(&self, field: &str, needle: &str) -> Value {
        self.derived("s", field, needle, |s| s.searched_json(field, needle))
    }

    pub fn order(&self, field: &str) -> Value {
        self.derived("o", field, "", |s| s.sorted_json(field))
    }

    pub fn create(&self, v: Value) -> Option<Value> {
        let given = match v.get("id") {
            Value::Null => None,
            id => Some(id.as_key()),
        };
        let (key, row) = match given {
            Some(key) => (key, as_row(v)),
            None => (String::new(), v),
        };
        let mut s = self.snap.write().unwrap();
        let (key, row) = if key.is_empty() {
            let mut id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
            while s.by_id.contains_key(&id.to_string()) {
                id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
            }
            (id.to_string(), with_id(row, id as f64))
        } else {
            if s.by_id.contains_key(&key) {
                return None;
            }
            (key, row)
        };
        let rows = Arc::make_mut(&mut s.rows);
        rows.push(row.clone());
        let idx = rows.len() - 1;
        s.by_id.insert(key, idx);
        let reuse = s.list_used.load(Ordering::Relaxed).then(|| s.all_json.take()).flatten();
        s.invalidate();
        s.all_json = reuse;
        s.append_json(&row);
        s.list_used.store(false, Ordering::Relaxed);
        self.bump();
        self.touch();
        Some(row)
    }

    pub fn update(&self, id: &str, patch: Value) -> Option<Value> {
        let mut s = self.snap.write().unwrap();
        let i = *s.by_id.get(id)?;
        let merged = as_row(merge(&s.rows[i], &patch));
        Arc::make_mut(&mut s.rows)[i] = merged.clone();
        s.invalidate();
        self.bump();
        self.touch();
        Some(merged)
    }

    pub fn delete(&self, id: &str) -> bool {
        let mut s = self.snap.write().unwrap();
        let Some(i) = s.by_id.remove(id) else { return false };
        Arc::make_mut(&mut s.rows)[i] = Value::Null;
        s.holes += 1;
        if s.holes * 2 > s.rows.len() {
            s.compact();
        }
        s.invalidate();
        self.bump();
        self.touch();
        true
    }

    pub fn reset(&self) {
        let mut s = self.snap.write().unwrap();
        s.rows = Arc::new(Vec::new());
        s.holes = 0;
        s.by_id.clear();
        s.invalidate();
        self.bump();
        self.next_id.store(0, Ordering::Relaxed);
        self.touch();
    }
}

#[derive(Default)]
pub struct Store {
    cols: Mutex<HashMap<String, Arc<Collection>>>,
    dirty: Arc<AtomicBool>,
}

impl Store {
    pub fn new() -> Arc<Store> {
        Arc::new(Store::default())
    }

    pub fn collection(&self, name: &str) -> Arc<Collection> {
        let mut cols = self.cols.lock().unwrap();
        if let Some(c) = cols.get(name) {
            return c.clone();
        }
        let c = Arc::new(Collection::new(name, self.dirty.clone()));
        cols.insert(name.to_string(), c.clone());
        c
    }

    pub fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = self.cols.lock().unwrap().keys().cloned().collect();
        out.sort();
        out
    }

    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    pub fn snapshot_json(&self) -> Vec<u8> {
        let mut names = self.names();
        names.sort();
        let mut out = Vec::with_capacity(1 << 12);
        out.push(b'{');
        for (i, name) in names.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            let col = self.collection(name);
            crate::value::write_string(&mut out, name);
            out.extend_from_slice(b":{\"next_id\":");
            crate::value::write_i64(&mut out, col.next_id() as i64);
            out.extend_from_slice(b",\"rows\":");
            Value::Arr(col.rows()).write_json(&mut out);
            out.push(b'}');
        }
        out.push(b'}');
        out
    }

    pub fn load_json(&self, raw: &[u8]) -> Result<(), String> {
        let Ok(Value::Obj(cols)) = crate::value::parse_json(raw) else {
            return Err("invalid store file".to_string());
        };
        for (name, entry) in cols.iter() {
            let rows = match entry.get("rows") {
                Value::Arr(rows) => rows.as_ref().clone(),
                _ => Vec::new(),
            };
            let next_id = match entry.get("next_id") {
                Value::Num(n) => n as u64,
                _ => rows.len() as u64,
            };
            self.collection(name).load(rows, next_id);
        }
        self.dirty.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.snapshot_json())?;
        std::fs::rename(&tmp, path)
    }

    pub fn load_file(&self, path: &std::path::Path) -> Result<(), String> {
        match std::fs::read(path) {
            Ok(raw) => self.load_json(&raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn autosave(self: &Arc<Self>, path: std::path::PathBuf, every: std::time::Duration) {
        let store = self.clone();
        let duty = std::env::var("VELO_SAVE_DUTY")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(10)
            .clamp(1, 100) as u32;
        std::thread::spawn(move || {
            let mut cost = std::time::Duration::ZERO;
            loop {
                std::thread::sleep(every.max(cost * (100 / duty).saturating_sub(1)));
                if store.take_dirty() {
                    let started = std::time::Instant::now();
                    if let Err(e) = store.save_to(&path) {
                        eprintln!("velo: save {}: {e}", path.display());
                    }
                    cost = started.elapsed();
                }
            }
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Agg {
    Sum,
    Avg,
    Min,
    Max,
}

impl Agg {
    fn name(self) -> &'static str {
        match self {
            Agg::Sum => "sum",
            Agg::Avg => "avg",
            Agg::Min => "min",
            Agg::Max => "max",
        }
    }
}

enum SortKey {
    Num(f64),
    Text(String),
}

fn sort_key(v: Option<&Value>) -> SortKey {
    match v {
        Some(Value::Num(n)) => SortKey::Num(*n),
        Some(other) => SortKey::Text(other.as_key()),
        None => SortKey::Text(String::new()),
    }
}

fn field_has(row: &Value, field: &str, lower_needle: &str) -> bool {
    match row.get_ref(field) {
        Some(Value::Str(s)) => s.to_lowercase().contains(lower_needle),
        Some(other) => other.as_key().to_lowercase().contains(lower_needle),
        None => lower_needle.is_empty(),
    }
}

fn field_eq(row: &Value, field: &str, want: &str) -> bool {
    match row.get_ref(field) {
        Some(v) => v.key_eq(want),
        None => want.is_empty(),
    }
}

fn as_row(v: Value) -> Value {
    match v {
        Value::Row(..) => v,
        Value::Obj(o) => Value::row(o.as_ref().clone()),
        other => other,
    }
}

fn with_id(v: Value, id: f64) -> Value {
    match v {
        Value::Obj(o) | Value::Row(o, _) => {
            let mut row: Obj = Vec::with_capacity(o.len() + 1);
            row.push((Arc::from("id"), Value::Num(id)));
            for (k, val) in o.iter() {
                if &**k != "id" {
                    row.push((k.clone(), val.clone()));
                }
            }
            Value::row(row)
        }
        other => Value::row(vec![(Arc::from("id"), Value::Num(id)), (Arc::from("value"), other)]),
    }
}

fn merge(base: &Value, patch: &Value) -> Value {
    let (Some(b), Some(p)) = (obj_of(base), obj_of(patch)) else {
        return match obj_of(base) {
            Some(_) => base.clone(),
            None => patch.clone(),
        };
    };
    let mut out: Obj = b.as_ref().clone();
    for (k, v) in p.iter() {
        if &**k == "id" {
            continue;
        }
        match out.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = v.clone(),
            None => out.push((k.clone(), v.clone())),
        }
    }
    Value::row(out)
}

fn obj_of(v: &Value) -> Option<&Arc<Obj>> {
    match v {
        Value::Obj(o) | Value::Row(o, _) => Some(o),
        _ => None,
    }
}
