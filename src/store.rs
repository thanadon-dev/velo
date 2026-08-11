use crate::value::{Obj, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

struct Snapshot {
    rows: Arc<Vec<Value>>,
    by_id: HashMap<String, usize>,
}

pub struct Collection {
    pub name: String,
    snap: RwLock<Snapshot>,
    next_id: AtomicU64,
    dirty: Arc<AtomicBool>,
}

impl Collection {
    fn new(name: &str, dirty: Arc<AtomicBool>) -> Collection {
        Collection {
            name: name.to_string(),
            snap: RwLock::new(Snapshot { rows: Arc::new(Vec::new()), by_id: HashMap::new() }),
            next_id: AtomicU64::new(0),
            dirty,
        }
    }

    fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn rows(&self) -> Arc<Vec<Value>> {
        self.snap.read().unwrap().rows.clone()
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
        s.rows = Arc::new(rows);
        self.next_id.store(next_id, Ordering::Relaxed);
    }

    pub fn all(&self) -> Value {
        Value::Arr(self.snap.read().unwrap().rows.clone())
    }

    pub fn count(&self) -> usize {
        self.snap.read().unwrap().rows.len()
    }

    pub fn find(&self, id: &str) -> Option<Value> {
        let s = self.snap.read().unwrap();
        s.by_id.get(id).map(|&i| s.rows[i].clone())
    }

    pub fn filter(&self, field: &str, want: &str) -> Value {
        let s = self.snap.read().unwrap();
        let rows: Vec<Value> = s
            .rows
            .iter()
            .filter(|r| r.get(field).as_key() == want)
            .cloned()
            .collect();
        Value::Arr(Arc::new(rows))
    }

    pub fn page(&self, offset: usize, limit: usize) -> Value {
        let s = self.snap.read().unwrap();
        let end = if limit == 0 {
            s.rows.len()
        } else {
            offset.saturating_add(limit).min(s.rows.len())
        };
        let rows = match s.rows.get(offset.min(s.rows.len())..end) {
            Some(slice) => slice.to_vec(),
            None => Vec::new(),
        };
        Value::Arr(Arc::new(rows))
    }

    pub fn create(&self, v: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let row = with_id(v, id as f64);
        let mut s = self.snap.write().unwrap();
        let rows = Arc::make_mut(&mut s.rows);
        rows.push(row.clone());
        let idx = rows.len() - 1;
        s.by_id.insert(id.to_string(), idx);
        self.touch();
        row
    }

    pub fn update(&self, id: &str, patch: Value) -> Option<Value> {
        let mut s = self.snap.write().unwrap();
        let i = *s.by_id.get(id)?;
        let merged = merge(&s.rows[i], &patch);
        Arc::make_mut(&mut s.rows)[i] = merged.clone();
        self.touch();
        Some(merged)
    }

    pub fn delete(&self, id: &str) -> bool {
        let mut s = self.snap.write().unwrap();
        let Some(i) = s.by_id.remove(id) else { return false };
        Arc::make_mut(&mut s.rows).remove(i);
        for v in s.by_id.values_mut() {
            if *v > i {
                *v -= 1;
            }
        }
        self.touch();
        true
    }

    pub fn reset(&self) {
        let mut s = self.snap.write().unwrap();
        s.rows = Arc::new(Vec::new());
        s.by_id.clear();
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
        std::thread::spawn(move || loop {
            std::thread::sleep(every);
            if store.take_dirty() {
                if let Err(e) = store.save_to(&path) {
                    eprintln!("velo: save {}: {e}", path.display());
                }
            }
        });
    }
}

fn with_id(v: Value, id: f64) -> Value {
    match v {
        Value::Obj(o) => {
            let mut row: Obj = Vec::with_capacity(o.len() + 1);
            row.push((Arc::from("id"), Value::Num(id)));
            for (k, val) in o.iter() {
                if &**k != "id" {
                    row.push((k.clone(), val.clone()));
                }
            }
            Value::obj(row)
        }
        other => Value::obj(vec![
            (Arc::from("id"), Value::Num(id)),
            (Arc::from("value"), other),
        ]),
    }
}

fn merge(base: &Value, patch: &Value) -> Value {
    let (Value::Obj(b), Value::Obj(p)) = (base, patch) else {
        return match base {
            Value::Obj(_) => base.clone(),
            _ => patch.clone(),
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
    Value::obj(out)
}
