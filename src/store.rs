use crate::value::{Obj, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

struct Snapshot {
    rows: Arc<Vec<Value>>,
    by_id: HashMap<String, usize>,
}

pub struct Collection {
    pub name: String,
    snap: RwLock<Snapshot>,
    next_id: AtomicU64,
}

impl Collection {
    fn new(name: &str) -> Collection {
        Collection {
            name: name.to_string(),
            snap: RwLock::new(Snapshot { rows: Arc::new(Vec::new()), by_id: HashMap::new() }),
            next_id: AtomicU64::new(0),
        }
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

    pub fn create(&self, v: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let row = with_id(v, id as f64);
        let mut s = self.snap.write().unwrap();
        let rows = Arc::make_mut(&mut s.rows);
        rows.push(row.clone());
        let idx = rows.len() - 1;
        s.by_id.insert(id.to_string(), idx);
        row
    }

    pub fn update(&self, id: &str, patch: Value) -> Option<Value> {
        let mut s = self.snap.write().unwrap();
        let i = *s.by_id.get(id)?;
        let merged = merge(&s.rows[i], &patch);
        Arc::make_mut(&mut s.rows)[i] = merged.clone();
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
        true
    }

    pub fn reset(&self) {
        let mut s = self.snap.write().unwrap();
        s.rows = Arc::new(Vec::new());
        s.by_id.clear();
        self.next_id.store(0, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Store {
    cols: Mutex<HashMap<String, Arc<Collection>>>,
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
        let c = Arc::new(Collection::new(name));
        cols.insert(name.to_string(), c.clone());
        c
    }

    pub fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = self.cols.lock().unwrap().keys().cloned().collect();
        out.sort();
        out
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
