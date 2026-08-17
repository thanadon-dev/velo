use crate::store::Cmp;
use crate::value::Value;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

pub struct Lock(#[allow(dead_code)] std::fs::File);

impl Lock {
    pub fn take(path: &Path) -> Result<Lock, String> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let taken =
            unsafe { flock(std::os::unix::io::AsRawFd::as_raw_fd(&file), LOCK_EX | LOCK_NB) };
        if taken != 0 {
            return Err(format!(
                "{} is held by another velo; one process owns a data file",
                path.display()
            ));
        }
        Ok(Lock(file))
    }
}

pub struct Wal {
    path: PathBuf,
    file: Mutex<std::fs::File>,
    sync: bool,
}

impl Wal {
    pub fn open(path: &Path) -> std::io::Result<Wal> {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        let sync = std::env::var("VELO_WAL_SYNC").is_ok_and(|v| v == "1" || v == "true");
        Ok(Wal { path: path.to_path_buf(), file: Mutex::new(file), sync })
    }

    fn append(&self, line: Vec<u8>) {
        let mut file = self.file.lock().unwrap();
        if let Err(e) = file.write_all(&line) {
            eprintln!("velo: wal {}: {e}", self.path.display());
            return;
        }
        if self.sync {
            let _ = file.sync_data();
        }
    }

    pub fn put(&self, coll: &str, row: &Value) {
        let mut line = Vec::with_capacity(128);
        line.extend_from_slice(b"{\"c\":");
        crate::value::write_string(&mut line, coll);
        line.extend_from_slice(b",\"p\":");
        row.write_json(&mut line);
        line.extend_from_slice(b"}\n");
        self.append(line);
    }

    pub fn merge(&self, coll: &str, id: &str, patch: &Value) {
        let mut line = Vec::with_capacity(96);
        line.extend_from_slice(b"{\"c\":");
        crate::value::write_string(&mut line, coll);
        line.extend_from_slice(b",\"u\":");
        crate::value::write_string(&mut line, id);
        line.extend_from_slice(b",\"f\":");
        patch.write_json(&mut line);
        line.extend_from_slice(b"}\n");
        self.append(line);
    }

    pub fn delete(&self, coll: &str, id: &str) {
        let mut line = Vec::with_capacity(64);
        line.extend_from_slice(b"{\"c\":");
        crate::value::write_string(&mut line, coll);
        line.extend_from_slice(b",\"d\":");
        crate::value::write_string(&mut line, id);
        line.extend_from_slice(b"}\n");
        self.append(line);
    }

    pub fn delete_where(&self, coll: &str, field: &str, op: Cmp, want: &str) {
        let mut line = Vec::with_capacity(96);
        line.extend_from_slice(b"{\"c\":");
        crate::value::write_string(&mut line, coll);
        line.extend_from_slice(b",\"w\":");
        crate::value::write_string(&mut line, field);
        line.extend_from_slice(b",\"o\":");
        crate::value::write_string(&mut line, op.name());
        line.extend_from_slice(b",\"v\":");
        crate::value::write_string(&mut line, want);
        line.extend_from_slice(b"}\n");
        self.append(line);
    }

    pub fn clear(&self, coll: &str) {
        let mut line = Vec::with_capacity(32);
        line.extend_from_slice(b"{\"c\":");
        crate::value::write_string(&mut line, coll);
        line.extend_from_slice(b",\"x\":true}\n");
        self.append(line);
    }

    pub fn len(&self) -> u64 {
        let file = self.file.lock().unwrap();
        file.metadata().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn drop_prefix(&self, upto: u64) {
        let mut file = self.file.lock().unwrap();
        let end = match file.metadata() {
            Ok(m) => m.len(),
            Err(e) => {
                eprintln!("velo: wal {}: {e}", self.path.display());
                return;
            }
        };
        if upto == 0 || upto > end {
            return;
        }
        let tail = match tail_from(&self.path, upto, end) {
            Ok(tail) => tail,
            Err(e) => {
                eprintln!("velo: wal {}: {e}", self.path.display());
                return;
            }
        };
        let rewritten =
            std::fs::OpenOptions::new().write(true).truncate(true).open(&self.path).and_then(
                |mut f| {
                    f.write_all(&tail)?;
                    f.seek(SeekFrom::End(0))?;
                    Ok(f)
                },
            );
        match rewritten {
            Ok(f) => *file = f,
            Err(e) => eprintln!("velo: wal {}: {e}", self.path.display()),
        }
    }
}

fn tail_from(path: &Path, from: u64, end: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut tail = Vec::with_capacity((end - from) as usize);
    file.read_to_end(&mut tail)?;
    Ok(tail)
}

pub enum Entry {
    Put(String, Value),
    Merge(String, String, Value),
    Delete(String, String),
    DeleteWhere(String, String, Cmp, String),
    Clear(String),
}

pub fn read(path: &Path) -> Result<Vec<Entry>, String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut out = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = crate::value::parse_json(line) else { break };
        let coll = entry.get("c").as_key();
        if coll.is_empty() {
            break;
        }
        match entry.get("p") {
            Value::Null => {}
            row => {
                out.push(Entry::Put(coll, row));
                continue;
            }
        }
        match entry.get("u") {
            Value::Null => {}
            id => {
                out.push(Entry::Merge(coll, id.as_key(), entry.get("f")));
                continue;
            }
        }
        match entry.get("d") {
            Value::Null => {}
            id => {
                out.push(Entry::Delete(coll, id.as_key()));
                continue;
            }
        }
        if matches!(entry.get("x"), Value::Bool(true)) {
            out.push(Entry::Clear(coll));
            continue;
        }
        let field = entry.get("w").as_key();
        let Some(op) = Cmp::parse(&entry.get("o").as_key()) else { break };
        if field.is_empty() {
            break;
        }
        out.push(Entry::DeleteWhere(coll, field, op, entry.get("v").as_key()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("velo-wal-{}-{name}", std::process::id()))
    }

    fn row(id: &str) -> Value {
        crate::value::parse_json(format!("{{\"id\":\"{id}\"}}").as_bytes()).unwrap()
    }

    fn ids(path: &Path) -> Vec<String> {
        read(path)
            .unwrap()
            .iter()
            .map(|e| match e {
                Entry::Put(_, row) => row.get("id").as_key(),
                Entry::Delete(_, id) => format!("-{id}"),
                _ => "?".to_string(),
            })
            .collect()
    }

    #[test]
    fn drop_prefix_keeps_only_what_came_after_the_mark() {
        let path = scratch("prefix.log");
        let _ = std::fs::remove_file(&path);
        let wal = Wal::open(&path).unwrap();
        wal.put("users", &row("a"));
        wal.put("users", &row("b"));
        let mark = wal.len();
        wal.put("users", &row("c"));
        wal.delete("users", "a");
        wal.drop_prefix(mark);
        assert_eq!(ids(&path), ["c", "-a"], "everything before the mark goes, nothing after it");
        wal.put("users", &row("d"));
        assert_eq!(ids(&path), ["c", "-a", "d"], "a write after trimming lands at the end");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_kind_of_entry_survives_a_round_trip() {
        let path = scratch("kinds.log");
        let _ = std::fs::remove_file(&path);
        let wal = Wal::open(&path).unwrap();
        wal.put("users", &row("a"));
        wal.merge("users", "a", &row("a"));
        wal.delete("users", "b");
        wal.delete_where("users", "team", Cmp::Ne, "blue");
        wal.clear("sessions");
        let entries = read(&path).unwrap();
        assert_eq!(entries.len(), 5);
        assert!(matches!(&entries[0], Entry::Put(c, _) if c == "users"));
        assert!(matches!(&entries[1], Entry::Merge(c, id, patch)
                if c == "users" && id == "a" && patch.get("id").as_key() == "a"));
        assert!(matches!(&entries[2], Entry::Delete(_, id) if id == "b"));
        assert!(matches!(&entries[3], Entry::DeleteWhere(_, f, op, v)
                if f == "team" && *op == Cmp::Ne && v == "blue"));
        assert!(matches!(&entries[4], Entry::Clear(c) if c == "sessions"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_torn_last_line_stops_the_replay_rather_than_failing_it() {
        let path = scratch("torn.log");
        let _ = std::fs::remove_file(&path);
        let wal = Wal::open(&path).unwrap();
        wal.put("users", &row("a"));
        drop(wal);
        let mut raw = std::fs::read(&path).unwrap();
        raw.extend_from_slice(b"{\"c\":\"users\",\"p\":{\"id\":\"b\"");
        std::fs::write(&path, &raw).unwrap();
        assert_eq!(read(&path).unwrap().len(), 1, "a half-written tail goes, the rest stays");
        let _ = std::fs::remove_file(&path);
    }
}
