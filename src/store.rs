use crate::value::{Obj, Value};
use std::cmp::Ordering as Ordering2;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

const CACHE_MAX: usize = 32;

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

struct Limits {
    cache_bytes: usize,
    local_cache_bytes: usize,
    append_max: usize,
}

impl Limits {
    fn from_env() -> Limits {
        let read = |key: &str, fallback: usize| {
            std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(fallback)
        };
        Limits {
            cache_bytes: read("VELO_CACHE_BYTES", 8 << 20),
            local_cache_bytes: read("VELO_LOCAL_CACHE_BYTES", 1 << 20),
            append_max: read("VELO_APPEND_MAX", 512 << 10),
        }
    }
}

fn tag_for(mut id: u64) -> String {
    let mut out = String::with_capacity(8);
    loop {
        out.push((b'a' + (id % 26) as u8) as char);
        id /= 26;
        if id == 0 {
            return out;
        }
    }
}

const CHUNK: usize = 128;

#[derive(Default, Clone)]
pub struct Rows {
    chunks: Vec<Arc<Vec<Value>>>,
    len: usize,
}

impl Rows {
    fn from_vec(rows: Vec<Value>) -> Rows {
        let mut out = Rows::default();
        for row in rows {
            out.push(row);
        }
        out
    }

    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, i: usize) -> Option<&Value> {
        self.chunks.get(i / CHUNK)?.get(i % CHUNK)
    }

    fn set(&mut self, i: usize, row: Value) {
        if let Some(chunk) = self.chunks.get_mut(i / CHUNK) {
            if i % CHUNK < chunk.len() {
                Arc::make_mut(chunk)[i % CHUNK] = row;
            }
        }
    }

    fn push(&mut self, row: Value) {
        if self.chunks.last().map_or(true, |c| c.len() == CHUNK) {
            self.chunks.push(Arc::new(Vec::with_capacity(CHUNK)));
        }
        Arc::make_mut(self.chunks.last_mut().unwrap()).push(row);
        self.len += 1;
    }

    fn iter(&self) -> impl Iterator<Item = &Value> {
        self.chunks.iter().flat_map(|c| c.iter())
    }

    pub fn live(&self) -> impl Iterator<Item = &Value> {
        self.iter().filter(|r| is_live(r))
    }
}

struct Snapshot {
    rows: Rows,
    holes: usize,
    by_id: HashMap<String, usize>,
    all_json: Option<Arc<Vec<u8>>>,
    list_used: AtomicBool,
    cache: RwLock<Keyed<Arc<Vec<u8>>>>,
    rows_cache: RwLock<Keyed<Arc<Vec<Value>>>>,
    index: RwLock<HashMap<String, HashMap<String, Vec<u32>>>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

const INDEX_MIN: usize = 512;
const INDEX_FIELDS: usize = 8;
const INDEX_VALUES: usize = 65_536;

fn is_live(v: &Value) -> bool {
    !matches!(v, Value::Null)
}

impl Snapshot {
    fn live(&self) -> impl Iterator<Item = &Value> {
        self.rows.live()
    }

    fn len(&self) -> usize {
        self.rows.len() - self.holes
    }

    fn candidates(&self, field: &str, want: &str) -> Option<Vec<u32>> {
        if self.rows.len() < INDEX_MIN {
            return None;
        }
        {
            let map = self.index.read().unwrap();
            if let Some(by_value) = map.get(field) {
                return Some(by_value.get(want).cloned().unwrap_or_default());
            }
            if map.len() >= INDEX_FIELDS {
                return None;
            }
        }
        let mut by_value: HashMap<String, Vec<u32>> = HashMap::new();
        let mut at = 0;
        for (i, row) in self.rows.iter().enumerate() {
            if is_live(row) {
                by_value.entry(index_key(row, field, &mut at)).or_default().push(i as u32);
            }
        }
        let hits = by_value.get(want).cloned().unwrap_or_default();
        if by_value.len() <= INDEX_VALUES {
            let mut map = self.index.write().unwrap();
            if map.len() < INDEX_FIELDS {
                map.insert(field.to_string(), by_value);
            }
        }
        Some(hits)
    }

    fn group_counts(&self, field: &str) -> Option<Vec<(String, u64)>> {
        self.candidates(field, "")?;
        let map = self.index.read().unwrap();
        let by_value = map.get(field)?;
        if by_value.get("").is_some_and(|rows| !rows.is_empty()) {
            return None;
        }
        let mut out: Vec<(String, u64)> = by_value
            .iter()
            .filter(|(value, _)| !value.is_empty())
            .map(|(value, rows)| (value.clone(), rows.len() as u64))
            .collect();
        out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Some(out)
    }

    fn holds(&self, field: &str, want: &str, skip: usize) -> bool {
        let same = |row: &Value| is_live(row) && row.get(field).as_key() == want;
        match self.candidates(field, want) {
            Some(hits) => hits
                .iter()
                .any(|i| *i as usize != skip && self.rows.get(*i as usize).is_some_and(same)),
            None => self.rows.iter().enumerate().any(|(i, row)| i != skip && same(row)),
        }
    }

    fn extend_index(&mut self, at: usize, row: &Value) {
        for (field, by_value) in self.index.get_mut().unwrap().iter_mut() {
            let mut seen = 0;
            by_value.entry(index_key(row, field, &mut seen)).or_default().push(at as u32);
        }
    }

    fn compact(&mut self) {
        if self.holes == 0 {
            return;
        }
        self.rows = Rows::from_vec(self.rows.live().cloned().collect());
        self.holes = 0;
        self.by_id.clear();
        for (i, row) in self.rows.iter().enumerate() {
            self.by_id.insert(row.get("id").as_key(), i);
        }
    }

    fn append_json(&mut self, row: &Value, append_max: usize) {
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
        if old.len() > append_max {
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
        self.index.get_mut().unwrap().clear();
        self.all_json = None;
        self.list_used.store(false, Ordering::Relaxed);
        self.cache.get_mut().unwrap().clear();
        self.rows_cache.get_mut().unwrap().clear();
    }

    fn cached_rows(&self, key: &str) -> Option<Arc<Vec<Value>>> {
        let hit = self.rows_cache.read().unwrap().get(key).cloned();
        let counter = if hit.is_some() { &self.hits } else { &self.misses };
        counter.fetch_add(1, Ordering::Relaxed);
        hit
    }

    fn store_rows(&self, key: &str, rows: Arc<Vec<Value>>, budget: usize) {
        let size = rows.len() * std::mem::size_of::<Value>();
        if size > budget {
            return;
        }
        let mut cache = self.rows_cache.write().unwrap();
        let used: usize = cache.values().map(|v| v.len() * std::mem::size_of::<Value>()).sum();
        if cache.len() >= CACHE_MAX || used + size > budget {
            cache.clear();
        }
        cache.insert(key.to_string(), rows);
    }

    fn cached(&self, key: &str) -> Option<Arc<Vec<u8>>> {
        let hit = self.cache.read().unwrap().get(key).cloned();
        let counter = if hit.is_some() { &self.hits } else { &self.misses };
        counter.fetch_add(1, Ordering::Relaxed);
        hit
    }

    fn store_cached(&self, key: &str, json: Arc<Vec<u8>>, budget: usize) -> Arc<Vec<u8>> {
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
}

static NEXT_COLLECTION_ID: AtomicU64 = AtomicU64::new(0);

type Keyed<V> = HashMap<String, V, std::hash::BuildHasherDefault<crate::router::Fnv>>;
type LocalCache = std::cell::RefCell<Keyed<(u64, Arc<Vec<u8>>)>>;

thread_local! {
    static LOCAL_CACHE: LocalCache = std::cell::RefCell::new(HashMap::default());
}

const LOCAL_CACHE_MAX: usize = 64;

pub struct Collection {
    pub name: String,
    tag: String,
    limits: Limits,
    version: AtomicU64,
    snap: RwLock<Snapshot>,
    next_id: AtomicU64,
    dirty: Arc<AtomicBool>,
    wal: Arc<std::sync::OnceLock<crate::wal::Wal>>,
}

impl Collection {
    fn new(
        name: &str,
        dirty: Arc<AtomicBool>,
        wal: Arc<std::sync::OnceLock<crate::wal::Wal>>,
    ) -> Collection {
        Collection {
            name: name.to_string(),
            tag: tag_for(NEXT_COLLECTION_ID.fetch_add(1, Ordering::Relaxed)),
            limits: Limits::from_env(),
            version: AtomicU64::new(0),
            snap: RwLock::new(Snapshot {
                rows: Rows::default(),
                holes: 0,
                by_id: HashMap::new(),
                all_json: None,
                list_used: AtomicBool::new(false),
                cache: RwLock::new(HashMap::default()),
                rows_cache: RwLock::new(HashMap::default()),
                index: RwLock::new(HashMap::new()),
                hits: AtomicU64::new(0),
                misses: AtomicU64::new(0),
            }),
            next_id: AtomicU64::new(0),
            dirty,
            wal,
        }
    }

    fn log(&self) -> Option<&crate::wal::Wal> {
        self.wal.get()
    }

    fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn bump(&self) {
        self.version.fetch_add(1, Ordering::Release);
    }

    fn derived(
        &self,
        kind: &str,
        a: &str,
        b: &str,
        build: impl FnOnce(&Rows) -> Arc<Vec<u8>>,
    ) -> Value {
        self.derived_with(kind, a, b, |_| (), |rows, ()| build(rows))
    }

    fn derived_with<T>(
        &self,
        kind: &str,
        a: &str,
        b: &str,
        pick: impl FnOnce(&Snapshot) -> T,
        build: impl FnOnce(&Rows, T) -> Arc<Vec<u8>>,
    ) -> Value {
        let version = self.version.load(Ordering::Acquire);
        let tag = &self.tag;

        let local = with_key4(tag, kind, a, b, |key| {
            LOCAL_CACHE
                .with(|c| c.borrow().get(key).and_then(|(v, j)| (*v == version).then(|| j.clone())))
        });
        if let Some(json) = local {
            return Value::Raw(json);
        }

        let shared = with_key4(tag, kind, a, b, |key| self.snap.read().unwrap().cached(key));
        let json = match shared {
            Some(json) => json,
            None => {
                let (rows, extra) = {
                    let s = self.snap.read().unwrap();
                    (s.rows.clone(), pick(&s))
                };
                let json = build(&rows, extra);
                with_key4(tag, kind, a, b, |key| {
                    let s = self.snap.read().unwrap();
                    if self.version.load(Ordering::Acquire) == version {
                        s.store_cached(key, json.clone(), self.limits.cache_bytes);
                    }
                });
                json
            }
        };

        with_key4(tag, kind, a, b, |key| {
            LOCAL_CACHE.with(|c| {
                let mut c = c.borrow_mut();
                let used: usize = c.values().map(|(_, v)| v.len()).sum();
                if c.len() >= LOCAL_CACHE_MAX || used + json.len() > self.limits.local_cache_bytes {
                    c.clear();
                }
                if json.len() <= self.limits.local_cache_bytes {
                    c.insert(key.to_string(), (version, json.clone()));
                }
            })
        });
        Value::Raw(json)
    }

    pub fn live_rows(&self) -> Vec<Value> {
        let s = self.snap.read().unwrap();
        s.live().cloned().collect()
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
        s.rows = Rows::from_vec(rows.into_iter().map(as_row).collect());
        s.holes = 0;
        s.invalidate();
        self.bump();
        self.next_id.store(next_id, Ordering::Relaxed);
    }

    pub fn all(&self) -> Value {
        let version = self.version.load(Ordering::Acquire);
        {
            let s = self.snap.read().unwrap();
            if let Some(json) = s.all_json.clone() {
                s.list_used.store(true, Ordering::Relaxed);
                return Value::Raw(json);
            }
        }
        let json = {
            let rows = {
                let s = self.snap.read().unwrap();
                s.rows.clone()
            };
            let mut out = Vec::with_capacity(rows.len() * 64 + 2);
            out.push(b'[');
            for (n, row) in rows.live().enumerate() {
                if n > 0 {
                    out.push(b',');
                }
                row.write_json(&mut out);
            }
            out.push(b']');
            Arc::new(out)
        };
        let mut s = self.snap.write().unwrap();
        if self.version.load(Ordering::Acquire) == version && s.all_json.is_none() {
            s.all_json = Some(json.clone());
        }
        s.list_used.store(true, Ordering::Relaxed);
        Value::Raw(json)
    }

    pub fn count(&self) -> usize {
        self.snap.read().unwrap().len()
    }

    pub fn find(&self, id: &str) -> Option<Value> {
        let s = self.snap.read().unwrap();
        s.by_id.get(id).and_then(|&i| s.rows.get(i).cloned())
    }

    pub fn filter(&self, field: &str, want: &str) -> Value {
        self.derived("w", field, want, |rows| filtered_json(rows, field, want))
    }

    pub fn first(&self, field: &str, want: &str) -> Option<Value> {
        let s = self.snap.read().unwrap();
        let mut at = 0;
        let found = s.live().find(|r| field_eq(r, field, want, &mut at)).cloned();
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
        self.derived("a", op.name(), field, |rows| aggregate_json(rows, op, field))
    }

    pub fn search(&self, field: &str, needle: &str) -> Value {
        self.derived("s", field, needle, |rows| searched_json(rows, field, needle))
    }

    pub fn order(&self, field: &str) -> Value {
        self.derived("o", field, "", |rows| sorted_json(rows, field))
    }

    pub fn query(&self, stages: &[Stage]) -> Value {
        let key = chain_key(stages);
        self.derived_with(
            "q",
            &key,
            "",
            |s| plan_hits(s, stages),
            |rows, hits| rows_json(&run_stages_hit(rows, stages, hits)),
        )
    }

    pub fn query_rows(&self, stages: &[Stage]) -> Value {
        let key = chain_key(stages);
        let version = self.version.load(Ordering::Acquire);
        if let Some(rows) = self.snap.read().unwrap().cached_rows(&key) {
            return Value::Arr(rows);
        }
        let (rows, hits) = {
            let s = self.snap.read().unwrap();
            (s.rows.clone(), plan_hits(&s, stages))
        };
        let picked: Arc<Vec<Value>> =
            Arc::new(run_stages_hit(&rows, stages, hits).into_iter().cloned().collect());
        let s = self.snap.read().unwrap();
        if self.version.load(Ordering::Acquire) == version {
            s.store_rows(&key, picked.clone(), self.limits.cache_bytes);
        }
        Value::Arr(picked)
    }

    pub fn query_count(&self, stages: &[Stage]) -> Value {
        let key = chain_key(stages);
        self.derived_with(
            "qc",
            &key,
            "",
            |s| plan_hits(s, stages),
            |rows, hits| {
                let mut out = Vec::with_capacity(12);
                crate::value::write_i64(&mut out, run_stages_hit(rows, stages, hits).len() as i64);
                Arc::new(out)
            },
        )
    }

    pub fn query_select(&self, stages: &[Stage], fields: &[Arc<str>]) -> Value {
        let mut key = String::with_capacity(48);
        for f in fields {
            push_part(&mut key, f);
        }
        push_chain(&mut key, stages);
        self.derived_with(
            "qs",
            &key,
            "",
            |s| plan_hits(s, stages),
            |rows, hits| selected_json(&run_stages_hit(rows, stages, hits), fields),
        )
    }

    pub fn query_agg(&self, stages: &[Stage], op: Agg, field: &str) -> Value {
        let mut key = String::with_capacity(48);
        key.push_str(op.name());
        push_chain(&mut key, stages);
        self.derived_with(
            "qa",
            &key,
            field,
            |s| plan_hits(s, stages),
            |rows, hits| aggregate_over(run_stages_hit(rows, stages, hits).into_iter(), op, field),
        )
    }

    pub fn query_group(&self, stages: &[Stage], by: &str, op: Option<(Agg, Arc<str>)>) -> Value {
        let mut key = String::with_capacity(64);
        match &op {
            Some((agg, field)) => {
                key.push_str(agg.name());
                push_part(&mut key, field);
            }
            None => key.push_str("count"),
        }
        push_chain(&mut key, stages);
        let whole = stages.is_empty() && op.is_none();
        self.derived_with(
            "qg",
            &key,
            by,
            |s| {
                let counted = if whole { s.group_counts(by) } else { None };
                let hits = if counted.is_some() { None } else { plan_hits(s, stages) };
                (counted, hits)
            },
            |rows, (counted, hits)| match counted {
                Some(counts) => counts_json(&counts),
                None => {
                    let picked = run_stages_hit(rows, stages, hits);
                    group_json(&picked, by, op.as_ref().map(|(a, f)| (*a, &**f)))
                }
            },
        )
    }

    pub fn query_first(&self, stages: &[Stage]) -> Option<Value> {
        let (head, pick) = match stages.split_last() {
            Some((Stage::Order(field), rest)) => (rest, Some(&**field)),
            _ => (stages, None),
        };
        let s = self.snap.read().unwrap();
        let rows = run_stages_hit(&s.rows, head, plan_hits(&s, head));
        match pick {
            Some(field) => extreme(&rows, field),
            None => rows.first().map(|r| (*r).clone()),
        }
    }

    pub fn create(&self, v: Value, unique: &[&str]) -> Option<Value> {
        let given = match v.get("id") {
            Value::Null => None,
            id => Some(id.as_key()),
        };
        let (key, row) = match given {
            Some(key) => (key, as_row(v)),
            None => (String::new(), v),
        };
        let mut s = self.snap.write().unwrap();
        if collides(&s, &row, unique, usize::MAX) {
            return None;
        }
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
        Some(self.insert_locked(&mut s, key, row))
    }

    fn insert_locked(&self, s: &mut Snapshot, key: String, row: Value) -> Value {
        let row = self.insert_raw(s, key, row);
        if let Some(wal) = self.log() {
            wal.put(&self.name, &row);
        }
        row
    }

    fn insert_raw(&self, s: &mut Snapshot, key: String, row: Value) -> Value {
        s.rows.push(row.clone());
        let idx = s.rows.len() - 1;
        s.by_id.insert(key, idx);
        let appendable = s.list_used.load(Ordering::Relaxed);
        let reuse = appendable.then(|| s.all_json.take()).flatten();
        let keep = std::mem::take(s.index.get_mut().unwrap());
        s.invalidate();
        *s.index.get_mut().unwrap() = keep;
        s.extend_index(idx, &row);
        s.all_json = reuse;
        s.append_json(&row, self.limits.append_max);
        s.list_used.store(false, Ordering::Relaxed);
        self.bump();
        self.touch();
        row
    }

    pub fn clear(&self) -> usize {
        let mut s = self.snap.write().unwrap();
        let removed = s.len();
        s.rows = Rows::default();
        s.holes = 0;
        s.by_id.clear();
        s.invalidate();
        if let Some(wal) = self.log() {
            wal.clear(&self.name);
        }
        self.bump();
        self.next_id.store(0, Ordering::Relaxed);
        if removed > 0 {
            self.touch();
        }
        removed
    }

    pub fn delete_where(&self, field: &str, op: Cmp, want: &str) -> usize {
        let want_num = want.trim().parse::<f64>().ok();
        let mut s = self.snap.write().unwrap();
        let mut at = 0;
        let hits: Vec<String> = s
            .live()
            .filter(|r| field_cmp(r, field, op, want, want_num, &mut at))
            .map(|r| r.get("id").as_key())
            .collect();
        if hits.is_empty() {
            return 0;
        }
        for id in &hits {
            if let Some(i) = s.by_id.remove(id) {
                s.rows.set(i, Value::Null);
                s.holes += 1;
            }
        }
        if s.holes * 2 > s.rows.len() {
            s.compact();
        }
        s.invalidate();
        if let Some(wal) = self.log() {
            wal.delete_where(&self.name, field, op, want);
        }
        self.bump();
        self.touch();
        hits.len()
    }

    pub fn upsert(&self, id: &str, value: Value, unique: &[&str]) -> Option<Value> {
        let mut s = self.snap.write().unwrap();
        if let Some(&i) = s.by_id.get(id) {
            return self.merge_locked(&mut s, id, i, &value, unique);
        }
        let keyed = match &value {
            Value::Obj(o) | Value::Row(o, _) => {
                let mut fields: Obj = Vec::with_capacity(o.len() + 1);
                fields.push((crate::value::intern("id"), id_value(id)));
                for (k, v) in o.iter() {
                    if &**k != "id" {
                        fields.push((k.clone(), v.clone()));
                    }
                }
                Value::row(fields)
            }
            other => Value::row(vec![
                (crate::value::intern("id"), id_value(id)),
                (crate::value::intern("value"), other.clone()),
            ]),
        };
        if collides(&s, &keyed, unique, usize::MAX) {
            return None;
        }
        Some(self.insert_locked(&mut s, id.to_string(), keyed))
    }

    pub fn put(&self, id: &str, row: Value) {
        let row = as_row(row);
        let mut s = self.snap.write().unwrap();
        match s.by_id.get(id) {
            Some(&i) => {
                s.rows.set(i, row);
                s.invalidate();
                self.bump();
                self.touch();
            }
            None => {
                self.insert_locked(&mut s, id.to_string(), row);
            }
        }
    }

    pub fn sample_row(&self) -> Option<Value> {
        let s = self.snap.read().unwrap();
        let first = s.rows.live().next().cloned();
        first
    }

    pub fn sample_id(&self) -> Option<String> {
        let s = self.snap.read().unwrap();
        let first = s.rows.live().next().map(|row| row.get("id").as_key());
        first
    }

    pub fn has(&self, id: &str) -> bool {
        self.snap.read().unwrap().by_id.contains_key(id)
    }

    pub fn update(&self, id: &str, patch: Value, unique: &[&str]) -> Option<Value> {
        let mut s = self.snap.write().unwrap();
        let i = *s.by_id.get(id)?;
        self.merge_locked(&mut s, id, i, &patch, unique)
    }

    fn merge_locked(
        &self,
        s: &mut Snapshot,
        id: &str,
        i: usize,
        patch: &Value,
        unique: &[&str],
    ) -> Option<Value> {
        let merged = self.merge_raw(s, i, patch, unique)?;
        if let Some(wal) = self.log() {
            wal.merge(&self.name, id, patch);
        }
        Some(merged)
    }

    fn merge_raw(
        &self,
        s: &mut Snapshot,
        i: usize,
        patch: &Value,
        unique: &[&str],
    ) -> Option<Value> {
        let merged = as_row(merge(s.rows.get(i)?, patch));
        if collides(s, &merged, unique, i) {
            return None;
        }
        s.rows.set(i, merged.clone());
        s.invalidate();
        self.bump();
        self.touch();
        Some(merged)
    }

    pub fn incr(&self, id: &str, field: &str, by: f64, floor: Option<f64>) -> Incr {
        if field == "id" || field.is_empty() {
            return Incr::NotNumber;
        }
        let mut s = self.snap.write().unwrap();
        let Some(&i) = s.by_id.get(id) else {
            return Incr::Missing;
        };
        let Some(row) = s.rows.get(i) else {
            return Incr::Missing;
        };
        let next = match row.get(field) {
            Value::Null => by,
            Value::Num(n) => n + by,
            _ => return Incr::NotNumber,
        };
        if floor.is_some_and(|low| next < low) {
            return Incr::TooLow;
        }
        let patch = Value::row(vec![(crate::value::intern(field), Value::Num(next))]);
        let merged = as_row(merge(row, &patch));
        s.rows.set(i, merged.clone());
        s.invalidate();
        if let Some(wal) = self.log() {
            wal.merge(&self.name, id, &patch);
        }
        self.bump();
        self.touch();
        Incr::Done(merged)
    }

    pub fn delete(&self, id: &str) -> bool {
        let mut s = self.snap.write().unwrap();
        let Some(i) = s.by_id.remove(id) else { return false };
        s.rows.set(i, Value::Null);
        s.holes += 1;
        if s.holes * 2 > s.rows.len() {
            s.compact();
        }
        s.invalidate();
        if let Some(wal) = self.log() {
            wal.delete(&self.name, id);
        }
        self.bump();
        self.touch();
        true
    }
}

#[derive(Default)]
pub struct Store {
    cols: Mutex<HashMap<String, Arc<Collection>>>,
    dirty: Arc<AtomicBool>,
    wal: Arc<std::sync::OnceLock<crate::wal::Wal>>,
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
        let c = Arc::new(Collection::new(name, self.dirty.clone(), self.wal.clone()));
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
            Value::Arr(Arc::new(col.live_rows())).write_json(&mut out);
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

    pub fn wal(&self) -> Option<&crate::wal::Wal> {
        self.wal.get()
    }

    pub fn attach_wal(&self, path: &std::path::Path) -> Result<usize, String> {
        let entries = crate::wal::read(path)?;
        let replayed = entries.len();
        for entry in entries {
            match entry {
                crate::wal::Entry::Put(coll, row) => {
                    let id = row.get("id").as_key();
                    if !id.is_empty() {
                        self.collection(&coll).put(&id, row);
                    }
                }
                crate::wal::Entry::Merge(coll, id, patch) => {
                    self.collection(&coll).upsert(&id, patch, &[]);
                }
                crate::wal::Entry::Delete(coll, id) => {
                    self.collection(&coll).delete(&id);
                }
                crate::wal::Entry::DeleteWhere(coll, field, op, want) => {
                    self.collection(&coll).delete_where(&field, op, &want);
                }
                crate::wal::Entry::Clear(coll) => {
                    self.collection(&coll).clear();
                }
            }
        }
        let wal = crate::wal::Wal::open(path).map_err(|e| e.to_string())?;
        if self.wal.set(wal).is_err() {
            return Err("wal already attached".to_string());
        }
        Ok(replayed)
    }

    pub fn load_file(&self, path: &std::path::Path) -> Result<(), String> {
        match std::fs::read(path) {
            Ok(raw) => self.load_json(&raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn expire_now(self: &Arc<Self>, rules: &[(String, String)]) -> usize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0);
        let mut cut = String::with_capacity(16);
        crate::value::write_number_into(&mut cut, now);
        let mut gone = 0;
        for (name, field) in rules {
            gone += self.collection(name).delete_where(field, Cmp::Lt, &cut);
        }
        gone
    }

    pub fn autoexpire(self: &Arc<Self>, rules: Vec<(String, String)>, every: std::time::Duration) {
        if rules.is_empty() {
            return;
        }
        let store = self.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(every);
            store.expire_now(&rules);
        });
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
                    let mark = store.wal().map(|w| w.len()).unwrap_or(0);
                    match store.save_to(&path) {
                        Ok(()) => {
                            if let Some(wal) = store.wal() {
                                wal.drop_prefix(mark);
                            }
                        }
                        Err(e) => eprintln!("velo: save {}: {e}", path.display()),
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

enum SortKey<'a> {
    Num(f64),
    Text(std::borrow::Cow<'a, str>),
}

fn sort_key(v: Option<&Value>) -> SortKey<'_> {
    match v {
        Some(Value::Num(n)) => SortKey::Num(*n),
        Some(Value::Str(s)) => SortKey::Text(std::borrow::Cow::Borrowed(s)),
        Some(other) => SortKey::Text(std::borrow::Cow::Owned(other.as_key())),
        None => SortKey::Text(std::borrow::Cow::Borrowed("")),
    }
}

fn field_has(row: &Value, field: &str, lower_needle: &str, at: &mut usize) -> bool {
    match row.get_at(field, at) {
        Some(Value::Str(s)) => s.to_lowercase().contains(lower_needle),
        Some(other) => other.as_key().to_lowercase().contains(lower_needle),
        None => lower_needle.is_empty(),
    }
}

fn field_eq(row: &Value, field: &str, want: &str, at: &mut usize) -> bool {
    match row.get_at(field, at) {
        Some(v) => v.key_eq(want),
        None => want.is_empty(),
    }
}

fn filtered_json(rows: &Rows, field: &str, want: &str) -> Arc<Vec<u8>> {
    let mut out = Vec::with_capacity(256);
    out.push(b'[');
    let mut at = 0;
    for (n, row) in rows.live().filter(|r| field_eq(r, field, want, &mut at)).enumerate() {
        if n > 0 {
            out.push(b',');
        }
        row.write_json(&mut out);
    }
    out.push(b']');
    Arc::new(out)
}

fn searched_json(rows: &Rows, field: &str, needle: &str) -> Arc<Vec<u8>> {
    let lower = needle.to_lowercase();
    let mut out = Vec::with_capacity(256);
    out.push(b'[');
    let mut at = 0;
    for (n, row) in rows.live().filter(|r| field_has(r, field, &lower, &mut at)).enumerate() {
        if n > 0 {
            out.push(b',');
        }
        row.write_json(&mut out);
    }
    out.push(b']');
    Arc::new(out)
}

pub enum Stage {
    Where(Arc<str>, Cmp, Arc<str>),
    Search(Arc<str>, Arc<str>),
    Order(Arc<str>),
    Page(usize, usize),
}

pub enum Incr {
    Done(Value),
    Missing,
    NotNumber,
    TooLow,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
}

impl Cmp {
    pub fn parse(text: &str) -> Option<Cmp> {
        Some(match text {
            "==" | "=" => Cmp::Eq,
            "!=" => Cmp::Ne,
            "<" => Cmp::Lt,
            "<=" => Cmp::Le,
            ">" => Cmp::Gt,
            ">=" => Cmp::Ge,
            "in" => Cmp::In,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Cmp::Eq => "==",
            Cmp::Ne => "!=",
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
            Cmp::In => "in",
        }
    }

    fn mark(self) -> char {
        match self {
            Cmp::Eq => '=',
            Cmp::Ne => '!',
            Cmp::Lt => '<',
            Cmp::Le => '(',
            Cmp::Gt => '>',
            Cmp::Ge => ')',
            Cmp::In => '@',
        }
    }

    fn holds(self, ord: Ordering2) -> bool {
        match self {
            Cmp::Eq => ord == Ordering2::Equal,
            Cmp::Ne => ord != Ordering2::Equal,
            Cmp::Lt => ord == Ordering2::Less,
            Cmp::Le => ord != Ordering2::Greater,
            Cmp::Gt => ord == Ordering2::Greater,
            Cmp::Ge => ord != Ordering2::Less,
            Cmp::In => false,
        }
    }
}

fn field_cmp(
    row: &Value,
    field: &str,
    op: Cmp,
    want: &str,
    want_num: Option<f64>,
    at: &mut usize,
) -> bool {
    if op == Cmp::Eq || op == Cmp::Ne {
        return field_eq(row, field, want, at) == (op == Cmp::Eq);
    }
    if op == Cmp::In {
        return want.split(',').any(|part| field_eq(row, field, part.trim(), at));
    }
    let Some(value) = row.get_at(field, at) else { return false };
    let ord = match (value, want_num) {
        (Value::Num(n), Some(w)) => n.partial_cmp(&w),
        (Value::Str(s), Some(w)) => match s.trim().parse::<f64>() {
            Ok(n) => n.partial_cmp(&w),
            Err(_) => Some(s.as_ref().cmp(want)),
        },
        (Value::Str(s), None) => Some(s.as_ref().cmp(want)),
        (other, _) => Some(other.as_key().as_str().cmp(want)),
    };
    ord.is_some_and(|ord| op.holds(ord))
}

pub fn expire_rules(spec: Option<String>) -> Vec<(String, String)> {
    let Some(spec) = spec else { return Vec::new() };
    let mut out = Vec::new();
    for part in spec.split(',') {
        let Some((coll, field)) = part.trim().split_once('.') else { continue };
        let (coll, field) = (coll.trim(), field.trim());
        if coll.is_empty() || field.is_empty() {
            continue;
        }
        out.push((coll.to_string(), field.to_string()));
    }
    out
}

fn push_usize(key: &mut String, mut n: usize) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    loop {
        at -= 1;
        digits[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    key.push_str(std::str::from_utf8(&digits[at..]).unwrap_or("0"));
}

fn push_part(key: &mut String, part: &str) {
    push_usize(key, part.len());
    key.push(':');
    key.push_str(part);
}

fn push_chain(key: &mut String, stages: &[Stage]) {
    for stage in stages {
        match stage {
            Stage::Where(f, op, v) => {
                key.push('w');
                key.push(op.mark());
                push_part(key, f);
                push_part(key, v);
            }
            Stage::Search(f, v) => {
                key.push('s');
                push_part(key, f);
                push_part(key, v);
            }
            Stage::Order(f) => {
                key.push('o');
                push_part(key, f);
            }
            Stage::Page(o, l) => {
                key.push('p');
                push_usize(key, *o);
                key.push(':');
                push_usize(key, *l);
            }
        }
    }
}

fn chain_key(stages: &[Stage]) -> String {
    let mut key = String::with_capacity(48);
    push_chain(&mut key, stages);
    key
}

fn collides(s: &Snapshot, row: &Value, unique: &[&str], skip: usize) -> bool {
    unique.iter().any(|field| match row.get(field) {
        Value::Null => false,
        want => s.holds(field, &want.as_key(), skip),
    })
}

fn index_key(row: &Value, field: &str, at: &mut usize) -> String {
    row.get_at(field, at).map(|v| v.as_key()).unwrap_or_default()
}

fn plan_hits(s: &Snapshot, stages: &[Stage]) -> Option<Vec<u32>> {
    let mut hits: Option<Vec<u32>> = None;
    for stage in stages {
        match stage {
            Stage::Page(_, _) => break,
            Stage::Where(field, Cmp::In, want) => {
                let mut found = Vec::new();
                let mut indexed = true;
                for part in want.split(',') {
                    match s.candidates(field, part.trim()) {
                        Some(list) => found.extend(list),
                        None => {
                            indexed = false;
                            break;
                        }
                    }
                }
                if !indexed {
                    continue;
                }
                found.sort_unstable();
                found.dedup();
                hits = Some(match hits {
                    None => found,
                    Some(prev) => intersect(&prev, &found),
                });
                if hits.as_ref().is_some_and(|h| h.is_empty()) {
                    break;
                }
            }
            Stage::Where(field, Cmp::Eq, want) => {
                let Some(found) = s.candidates(field, want) else { continue };
                hits = Some(match hits {
                    None => found,
                    Some(prev) => intersect(&prev, &found),
                });
                if hits.as_ref().is_some_and(|h| h.is_empty()) {
                    break;
                }
            }
            _ => {}
        }
    }
    hits
}

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering2::Less => i += 1,
            Ordering2::Greater => j += 1,
            Ordering2::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

fn run_stages_hit<'a>(rows: &'a Rows, stages: &[Stage], hits: Option<Vec<u32>>) -> Vec<&'a Value> {
    let Some(hits) = hits else { return run_stages(rows, stages) };
    let mut cur: Vec<&Value> =
        hits.iter().filter_map(|&i| rows.get(i as usize)).filter(|r| is_live(r)).collect();
    apply_stages(&mut cur, &stages[1..]);
    cur
}

fn run_stages<'a>(rows: &'a Rows, stages: &[Stage]) -> Vec<&'a Value> {
    let mut cur: Vec<&Value> = rows.live().collect();
    apply_stages(&mut cur, stages);
    cur
}

fn apply_stages(cur: &mut Vec<&Value>, stages: &[Stage]) {
    for (at, stage) in stages.iter().enumerate() {
        match stage {
            Stage::Where(f, op, v) => {
                let want_num = v.trim().parse::<f64>().ok();
                let mut seen = 0;
                cur.retain(|r| field_cmp(r, f, *op, v, want_num, &mut seen));
            }
            Stage::Search(f, needle) => {
                let lower = needle.to_lowercase();
                let mut seen = 0;
                cur.retain(|r| field_has(r, f, &lower, &mut seen));
            }
            Stage::Order(f) => {
                let top = match stages.get(at + 1) {
                    Some(Stage::Page(offset, limit)) if *limit > 0 => offset.checked_add(*limit),
                    _ => None,
                };
                sort_rows_top(cur, f, top);
            }
            Stage::Page(offset, limit) => {
                let take = if *limit == 0 { usize::MAX } else { *limit };
                *cur = cur.drain(..).skip(*offset).take(take).collect();
            }
        }
    }
}

fn selected_json(rows: &[&Value], fields: &[Arc<str>]) -> Arc<Vec<u8>> {
    if crate::value::any_nested(fields) {
        let plan = crate::value::keep_plan(fields);
        let mut out = Vec::with_capacity(rows.len() * (fields.len() * 16 + 4) + 2);
        out.push(b'[');
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            crate::value::keep_object_planned(row, &plan).write_json(&mut out);
        }
        out.push(b']');
        return Arc::new(out);
    }
    let mut out = Vec::with_capacity(rows.len() * (fields.len() * 16 + 4) + 2);
    out.push(b'[');
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.push(b'{');
        let mut first = true;
        for field in fields {
            let mut at = 0;
            let Some(value) = row.get_at(field, &mut at) else { continue };
            if !first {
                out.push(b',');
            }
            first = false;
            crate::value::write_string(&mut out, field);
            out.push(b':');
            value.write_json(&mut out);
        }
        out.push(b'}');
    }
    out.push(b']');
    Arc::new(out)
}

fn rows_json(rows: &[&Value]) -> Arc<Vec<u8>> {
    let mut out = Vec::with_capacity(rows.len() * 64 + 2);
    out.push(b'[');
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        row.write_json(&mut out);
    }
    out.push(b']');
    Arc::new(out)
}

fn aggregate_json(rows: &Rows, op: Agg, field: &str) -> Arc<Vec<u8>> {
    aggregate_over(rows.live(), op, field)
}

fn aggregate_over<'a>(rows: impl Iterator<Item = &'a Value>, op: Agg, field: &str) -> Arc<Vec<u8>> {
    let mut acc: Option<f64> = None;
    let mut n = 0u64;
    let mut at = 0;
    for row in rows {
        let Some(Value::Num(v)) = row.get_at(field, &mut at) else { continue };
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
    Arc::new(out)
}

fn counts_json(counts: &[(String, u64)]) -> Arc<Vec<u8>> {
    let mut out = Vec::with_capacity(counts.len() * 24 + 2);
    out.push(b'{');
    for (i, (value, rows)) in counts.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        crate::value::write_string(&mut out, value);
        out.push(b':');
        crate::value::write_i64(&mut out, *rows as i64);
    }
    out.push(b'}');
    Arc::new(out)
}

#[derive(Default)]
struct Group {
    rows: u64,
    nums: u64,
    acc: Option<f64>,
}

fn group_json(rows: &[&Value], by: &str, op: Option<(Agg, &str)>) -> Arc<Vec<u8>> {
    let mut groups: Keyed<Group> = HashMap::default();
    let mut at = 0;
    let mut seen = 0;
    for row in rows {
        let key = match row.get_at(by, &mut at) {
            None | Some(Value::Null) => continue,
            Some(v) => v.as_key_ref(),
        };
        let slot = match groups.get_mut(&*key) {
            Some(slot) => slot,
            None => groups.entry(key.into_owned()).or_default(),
        };
        slot.rows += 1;
        let Some((agg, field)) = op else { continue };
        let Some(Value::Num(v)) = row.get_at(field, &mut seen) else { continue };
        slot.nums += 1;
        slot.acc = Some(match (slot.acc, agg) {
            (None, _) => *v,
            (Some(a), Agg::Sum | Agg::Avg) => a + v,
            (Some(a), Agg::Min) => a.min(*v),
            (Some(a), Agg::Max) => a.max(*v),
        });
    }
    let mut keys: Vec<&String> = groups.keys().collect();
    keys.sort_unstable();
    let mut out = Vec::with_capacity(keys.len() * 24 + 2);
    out.push(b'{');
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        crate::value::write_string(&mut out, key);
        out.push(b':');
        let g = &groups[*key];
        match (op, g.acc) {
            (None, _) => crate::value::write_i64(&mut out, g.rows as i64),
            (Some((Agg::Avg, _)), Some(a)) => {
                crate::value::write_number(&mut out, a / g.nums as f64)
            }
            (Some(_), Some(a)) => crate::value::write_number(&mut out, a),
            (Some((Agg::Sum, _)), None) => out.extend_from_slice(b"0"),
            (Some(_), None) => out.extend_from_slice(b"null"),
        }
    }
    out.push(b'}');
    Arc::new(out)
}

fn cmp_keys(a: &SortKey, b: &SortKey) -> Ordering2 {
    match (a, b) {
        (SortKey::Num(m), SortKey::Num(n)) => m.partial_cmp(n).unwrap_or(Ordering2::Equal),
        (SortKey::Num(_), _) => Ordering2::Less,
        (_, SortKey::Num(_)) => Ordering2::Greater,
        (SortKey::Text(m), SortKey::Text(n)) => m.cmp(n),
    }
}

fn extreme(rows: &[&Value], field: &str) -> Option<Value> {
    let (sort_field, desc) = match field.strip_prefix('-') {
        Some(f) => (f, true),
        None => (field, false),
    };
    let want = if desc { Ordering2::Greater } else { Ordering2::Less };
    let mut best: Option<(SortKey, &Value)> = None;
    let mut at = 0;
    for row in rows {
        let key = sort_key(row.get_at(sort_field, &mut at));
        let better = match &best {
            Some((seen, _)) => cmp_keys(&key, seen) == want,
            None => true,
        };
        if better {
            best = Some((key, row));
        }
    }
    best.map(|(_, row)| row.clone())
}

fn sort_rows(rows: &mut Vec<&Value>, field: &str) {
    sort_rows_top(rows, field, None)
}

fn sort_rows_top<'a>(rows: &mut Vec<&'a Value>, field: &str, top: Option<usize>) {
    let (sort_field, desc) = match field.strip_prefix('-') {
        Some(f) => (f, true),
        None => (field, false),
    };
    let mut seen = 0;
    let mut keyed: Vec<(SortKey, usize, &Value)> = rows
        .iter()
        .enumerate()
        .map(|(at, r)| (sort_key(r.get_at(sort_field, &mut seen)), at, *r))
        .collect();
    let order = |a: &(SortKey<'a>, usize, &'a Value), b: &(SortKey<'a>, usize, &'a Value)| {
        let ord = cmp_keys(&a.0, &b.0);
        let ord = if desc { ord.reverse() } else { ord };
        ord.then(a.1.cmp(&b.1))
    };
    match top {
        Some(n) if n < keyed.len() => {
            keyed.select_nth_unstable_by(n, order);
            keyed.truncate(n);
            keyed.sort_unstable_by(order);
        }
        _ => keyed.sort_unstable_by(order),
    }
    rows.clear();
    rows.extend(keyed.into_iter().map(|(_, _, row)| row));
}

fn sorted_json(rows: &Rows, field: &str) -> Arc<Vec<u8>> {
    let mut cur: Vec<&Value> = rows.live().collect();
    sort_rows(&mut cur, field);
    rows_json(&cur)
}

fn id_value(id: &str) -> Value {
    match id.parse::<f64>() {
        Ok(n) if n.fract() == 0.0 && id.parse::<i64>().is_ok() => Value::Num(n),
        _ => Value::str(id),
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
            let mut fields = Arc::try_unwrap(o).unwrap_or_else(|shared| (*shared).clone());
            fields.retain(|(k, _)| &**k != "id");
            fields.insert(0, (crate::value::intern("id"), Value::Num(id)));
            Value::row(fields)
        }
        other => Value::row(vec![
            (crate::value::intern("id"), Value::Num(id)),
            (crate::value::intern("value"), other),
        ]),
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
            Some(slot) => slot.1 = deep(&slot.1, v),
            None => out.push((k.clone(), v.clone())),
        }
    }
    Value::row(out)
}

fn deep(base: &Value, patch: &Value) -> Value {
    let (Some(b), Some(p)) = (obj_of(base), obj_of(patch)) else { return patch.clone() };
    let mut out: Obj = b.as_ref().clone();
    for (k, v) in p.iter() {
        match out.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = deep(&slot.1, v),
            None => out.push((k.clone(), v.clone())),
        }
    }
    Value::obj(out)
}

fn obj_of(v: &Value) -> Option<&Arc<Obj>> {
    match v {
        Value::Obj(o) | Value::Row(o, _) => Some(o),
        _ => None,
    }
}

pub fn attach(v: &Value, key: &str, other: &Collection, out: &str) -> Value {
    match v {
        Value::Arr(items) => {
            Value::Arr(Arc::new(items.iter().map(|i| attach(i, key, other, out)).collect()))
        }
        Value::Obj(o) | Value::Row(o, _) => {
            let mut at = 0;
            let found = match v.get_at(key, &mut at) {
                None | Some(Value::Null) => Value::Null,
                Some(want) => other.find(&want.as_key_ref()).unwrap_or(Value::Null),
            };
            let mut fields: crate::value::Obj = Vec::with_capacity(o.len() + 1);
            let mut placed = false;
            for (k, val) in o.iter() {
                match &**k == out {
                    true => {
                        fields.push((k.clone(), found.clone()));
                        placed = true;
                    }
                    false => fields.push((k.clone(), val.clone())),
                }
            }
            if !placed {
                fields.push((crate::value::intern(out), found));
            }
            Value::obj(fields)
        }
        plain => plain.clone(),
    }
}

pub enum Step {
    Create(Value, Vec<Arc<str>>),
    Update(String, Value, Vec<Arc<str>>),
    Upsert(String, Value, Vec<Arc<str>>),
    Incr(String, Arc<str>, f64, Option<f64>),
    Delete(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    Conflict,
    Missing,
    NotNumber,
    TooLow,
}

enum Undo {
    Inserted(String, usize),
    Replaced(usize, Value),
    Removed(String, usize, Value),
}

enum Logged {
    Put(Value),
    Merge(String, Value),
    Delete(String),
}

impl Collection {
    fn apply_tx(
        &self,
        s: &mut Snapshot,
        step: Step,
        undo: &mut Vec<Undo>,
        log: &mut Vec<Logged>,
    ) -> Result<Value, Refused> {
        match step {
            Step::Create(v, unique) => {
                let names: Vec<&str> = unique.iter().map(|n| &**n).collect();
                let given = match v.get("id") {
                    Value::Null => None,
                    id => Some(id.as_key()),
                };
                let (key, row) = match given {
                    Some(key) => (key, as_row(v)),
                    None => (String::new(), v),
                };
                if collides(s, &row, &names, usize::MAX) {
                    return Err(Refused::Conflict);
                }
                let (key, row) = if key.is_empty() {
                    let mut id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
                    while s.by_id.contains_key(&id.to_string()) {
                        id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
                    }
                    (id.to_string(), with_id(row, id as f64))
                } else {
                    if s.by_id.contains_key(&key) {
                        return Err(Refused::Conflict);
                    }
                    (key, row)
                };
                let row = self.insert_raw(s, key.clone(), row);
                undo.push(Undo::Inserted(key, s.rows.len() - 1));
                log.push(Logged::Put(row.clone()));
                Ok(row)
            }
            Step::Update(id, patch, unique) => {
                let names: Vec<&str> = unique.iter().map(|n| &**n).collect();
                let Some(&i) = s.by_id.get(&id) else { return Err(Refused::Missing) };
                let Some(prev) = s.rows.get(i).cloned() else { return Err(Refused::Missing) };
                let merged = self.merge_raw(s, i, &patch, &names).ok_or(Refused::Conflict)?;
                undo.push(Undo::Replaced(i, prev));
                log.push(Logged::Merge(id, patch));
                Ok(merged)
            }
            Step::Upsert(id, value, unique) => {
                let names: Vec<&str> = unique.iter().map(|n| &**n).collect();
                if let Some(&i) = s.by_id.get(&id) {
                    let Some(prev) = s.rows.get(i).cloned() else { return Err(Refused::Missing) };
                    let merged = self.merge_raw(s, i, &value, &names).ok_or(Refused::Conflict)?;
                    undo.push(Undo::Replaced(i, prev));
                    log.push(Logged::Merge(id, value));
                    return Ok(merged);
                }
                let keyed = keyed_row(&id, &value);
                if collides(s, &keyed, &names, usize::MAX) {
                    return Err(Refused::Conflict);
                }
                let row = self.insert_raw(s, id.clone(), keyed);
                undo.push(Undo::Inserted(id, s.rows.len() - 1));
                log.push(Logged::Put(row.clone()));
                Ok(row)
            }
            Step::Incr(id, field, by, floor) => {
                if &*field == "id" || field.is_empty() {
                    return Err(Refused::NotNumber);
                }
                let Some(&i) = s.by_id.get(&id) else { return Err(Refused::Missing) };
                let Some(prev) = s.rows.get(i).cloned() else { return Err(Refused::Missing) };
                let next = match prev.get(&field) {
                    Value::Null => by,
                    Value::Num(n) => n + by,
                    _ => return Err(Refused::NotNumber),
                };
                if floor.is_some_and(|low| next < low) {
                    return Err(Refused::TooLow);
                }
                let patch = Value::row(vec![(crate::value::intern(&field), Value::Num(next))]);
                let Some(merged) = self.merge_raw(s, i, &patch, &[]) else {
                    return Err(Refused::Missing);
                };
                undo.push(Undo::Replaced(i, prev));
                log.push(Logged::Merge(id, patch));
                Ok(merged)
            }
            Step::Delete(id) => {
                let Some(&i) = s.by_id.get(&id) else { return Err(Refused::Missing) };
                let Some(prev) = s.rows.get(i).cloned() else { return Err(Refused::Missing) };
                s.by_id.remove(&id);
                s.rows.set(i, Value::Null);
                s.holes += 1;
                s.invalidate();
                self.bump();
                self.touch();
                undo.push(Undo::Removed(id.clone(), i, prev));
                log.push(Logged::Delete(id));
                Ok(Value::obj(vec![(crate::value::intern("deleted"), Value::Bool(true))]))
            }
        }
    }

    fn undo_tx(&self, s: &mut Snapshot, step: Undo) {
        match step {
            Undo::Inserted(key, i) => {
                s.by_id.remove(&key);
                s.rows.set(i, Value::Null);
                s.holes += 1;
            }
            Undo::Replaced(i, prev) => s.rows.set(i, prev),
            Undo::Removed(key, i, prev) => {
                s.rows.set(i, prev);
                s.by_id.insert(key, i);
                s.holes -= 1;
            }
        }
        s.invalidate();
        self.bump();
    }

    fn write_log(&self, entry: &Logged) {
        let Some(wal) = self.log() else { return };
        match entry {
            Logged::Put(row) => wal.put(&self.name, row),
            Logged::Merge(id, patch) => wal.merge(&self.name, id, patch),
            Logged::Delete(id) => wal.delete(&self.name, id),
        }
    }
}

fn keyed_row(id: &str, value: &Value) -> Value {
    match value {
        Value::Obj(o) | Value::Row(o, _) => {
            let mut fields: Obj = Vec::with_capacity(o.len() + 1);
            fields.push((crate::value::intern("id"), id_value(id)));
            for (k, v) in o.iter() {
                if &**k != "id" {
                    fields.push((k.clone(), v.clone()));
                }
            }
            Value::row(fields)
        }
        other => Value::row(vec![
            (crate::value::intern("id"), id_value(id)),
            (crate::value::intern("value"), other.clone()),
        ]),
    }
}

pub fn commit(plan: Vec<(Arc<Collection>, Step)>) -> Result<Vec<Value>, (usize, Refused)> {
    let mut cols: Vec<Arc<Collection>> = Vec::with_capacity(plan.len());
    for (col, _) in &plan {
        if !cols.iter().any(|held| Arc::ptr_eq(held, col)) {
            cols.push(col.clone());
        }
    }
    cols.sort_by(|a, b| a.tag.cmp(&b.tag));
    let mut held: Vec<_> = cols.iter().map(|c| c.snap.write().unwrap()).collect();

    let at = |col: &Arc<Collection>| cols.iter().position(|c| Arc::ptr_eq(c, col)).unwrap_or(0);
    let mut undo: Vec<(usize, Undo)> = Vec::with_capacity(plan.len());
    let mut log: Vec<(usize, Logged)> = Vec::with_capacity(plan.len());
    let mut out = Vec::with_capacity(plan.len());

    for (n, (col, step)) in plan.into_iter().enumerate() {
        let slot = at(&col);
        let mut mine = Vec::new();
        let mut logged = Vec::new();
        match col.apply_tx(&mut held[slot], step, &mut mine, &mut logged) {
            Ok(value) => {
                undo.extend(mine.into_iter().map(|u| (slot, u)));
                log.extend(logged.into_iter().map(|e| (slot, e)));
                out.push(value);
            }
            Err(why) => {
                for (slot, step) in undo.into_iter().rev() {
                    cols[slot].undo_tx(&mut held[slot], step);
                }
                return Err((n, why));
            }
        }
    }
    for (slot, entry) in &log {
        cols[*slot].write_log(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expire_rules_reads_a_list_of_collection_and_field() {
        assert_eq!(expire_rules(None), Vec::new());
        assert_eq!(expire_rules(Some(String::new())), Vec::new());
        assert_eq!(
            expire_rules(Some(" sessions.until , tokens.exp ".into())),
            vec![("sessions".into(), "until".into()), ("tokens".into(), "exp".into())]
        );
        assert_eq!(
            expire_rules(Some("nodot,.empty,coll.,ok.f".into())),
            vec![("ok".into(), "f".into())]
        );
    }

    #[test]
    fn expiring_takes_the_past_and_leaves_the_future() {
        let store = Store::new();
        let sessions = store.collection("sessions");
        let keep = store.collection("keep");
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
                as u64;
        for (id, until) in [("a", now - 1), ("b", now - 100_000), ("c", now + 100_000)] {
            let raw = format!(r#"{{"id":"{id}","until":{until}}}"#);
            sessions.create(crate::value::parse_json(raw.as_bytes()).unwrap(), &[]).unwrap();
            keep.create(crate::value::parse_json(raw.as_bytes()).unwrap(), &[]).unwrap();
        }
        let rules = expire_rules(Some("sessions.until".into()));
        assert_eq!(store.expire_now(&rules), 2);
        assert_eq!(sessions.count(), 1);
        assert!(sessions.find("c").is_some(), "the unexpired row survives");
        assert_eq!(keep.count(), 3, "a collection outside the rules is untouched");
        assert_eq!(store.expire_now(&rules), 0, "a second sweep finds nothing");
    }

    #[test]
    fn a_row_without_the_field_is_never_expired() {
        let store = Store::new();
        let c = store.collection("sessions");
        for raw in [r#"{"id":"a"}"#, r#"{"id":"b","until":"soon"}"#, r#"{"id":"c","until":1}"#] {
            c.create(crate::value::parse_json(raw.as_bytes()).unwrap(), &[]).unwrap();
        }
        let rules = expire_rules(Some("sessions.until".into()));
        assert_eq!(store.expire_now(&rules), 1, "only the numeric past goes");
        assert!(c.find("a").is_some() && c.find("b").is_some());
    }

    #[test]
    fn a_chain_key_pins_every_part_it_is_built_from() {
        let key = |stages: &[Stage]| {
            let mut out = String::new();
            push_chain(&mut out, stages);
            out
        };
        let mut seen = std::collections::HashSet::new();
        for n in [0usize, 1, 9, 10, 99, 100, 1_000, 12_345, usize::MAX] {
            let mut out = String::new();
            push_usize(&mut out, n);
            assert_eq!(out, n.to_string());
        }
        let plans: Vec<Vec<Stage>> = vec![
            vec![],
            vec![Stage::Where("a".into(), Cmp::Eq, "bc".into())],
            vec![Stage::Where("ab".into(), Cmp::Eq, "c".into())],
            vec![Stage::Where("a".into(), Cmp::Ne, "bc".into())],
            vec![Stage::Search("a".into(), "bc".into())],
            vec![Stage::Order("a".into())],
            vec![Stage::Page(1, 23)],
            vec![Stage::Page(12, 3)],
            vec![Stage::Order("a".into()), Stage::Page(1, 23)],
            vec![Stage::Page(1, 23), Stage::Order("a".into())],
            vec![Stage::Where("a".into(), Cmp::Eq, "b".into()), Stage::Order("c".into())],
        ];
        for plan in &plans {
            assert!(seen.insert(key(plan)), "two different plans share a cache key");
        }
    }
}
