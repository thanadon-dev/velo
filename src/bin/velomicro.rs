use std::time::Instant;
use velo::{compile, Server};

struct Report {
    json: bool,
    check: Option<String>,
    rows: Vec<(String, f64)>,
}

impl Report {
    fn add(&mut self, name: &str, per_op: f64, bytes: usize) {
        self.rows.push((name.to_string(), per_op * 1e6));
        if !self.json && self.check.is_none() {
            println!(
                "{:9} {:>9.2} us/op  {:>10.0} op/s  {} bytes",
                name,
                per_op * 1e6,
                1.0 / per_op,
                bytes
            );
        }
    }

    fn finish(&self) -> i32 {
        if let Some(path) = &self.check {
            return self.compare(path);
        }
        if !self.json {
            return 0;
        }
        let mut out = String::from("{");
        for (i, (name, us)) in self.rows.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{name}\":{us:.3}"));
        }
        out.push('}');
        println!("{out}");
        0
    }

    fn compare(&self, path: &str) -> i32 {
        let limit: f64 =
            std::env::var("VELO_PERF_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0);
        let raw = match std::fs::read(path) {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("velomicro: {path}: {e}");
                return 1;
            }
        };
        let Ok(velo::Value::Obj(base)) = velo::value::parse_json(&raw) else {
            eprintln!("velomicro: {path}: not a baseline object");
            return 1;
        };
        let mut slow = Vec::new();
        for (name, want) in base.iter() {
            let velo::Value::Num(want) = want else { continue };
            let Some((_, got)) = self.rows.iter().find(|(n, _)| n.as_str() == &**name) else {
                eprintln!("velomicro: baseline has {name}, this run does not");
                return 1;
            };
            let ratio = if *want > 0.0 { got / want } else { 1.0 };
            let mark = if ratio <= limit { "ok" } else { "SLOW" };
            println!("{name:16} baseline {want:8.2} us   now {got:8.2} us   x{ratio:.2}  {mark}");
            if ratio > limit {
                slow.push(name.to_string());
            }
        }
        if slow.is_empty() {
            println!("no performance regressions over {limit}x");
            return 0;
        }
        eprintln!("velomicro: slower than baseline: {}", slow.join(", "));
        1
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let check =
        args.iter().position(|a| a == "--check").and_then(|i| args.get(i + 1).cloned()).or_else(
            || args.iter().any(|a| a == "--check").then(|| "bench/baseline.json".to_string()),
        );
    let rows: usize =
        args.iter().find(|a| !a.starts_with("--")).and_then(|v| v.parse().ok()).unwrap_or(500);
    let quiet = json || check.is_some();
    let mut report = Report { json, check, rows: Vec::new() };
    let src = "GET /u => db.users.all()\n\
               GET /w => db.users.where(\"team\", query.t)\n\
               GET /o => db.users.order(\"name\")\n\
               GET /f => db.users.find(\"1\")\n\
               GET /c => db.users.where(\"team\", query.t).order(\"name\").page(0, 20)\n\
               GET /s => db.users.select(\"id\", \"name\")\n\
               POST /w => db.users.create(body)\n\
               DELETE /d/:id => db.users.delete(id)\n";
    let store = velo::Store::new();
    let prog = compile(src, Some(store.clone())).unwrap();
    let s = Server::new(prog).unwrap();
    let col = store.collection("users");
    for i in 0..rows {
        let v = velo::value::parse_json(format!("{{\"team\":\"x\",\"name\":\"u{i}\"}}").as_bytes())
            .unwrap();
        col.create(v, &[]).unwrap();
    }
    if !quiet {
        println!("{rows} rows");
    }
    {
        let t0 = Instant::now();
        let n = 2_000;
        let mut out = Vec::new();
        for i in 0..n {
            out.clear();
            let body = format!("{{\"team\":\"x\",\"name\":\"o{i}\"}}");
            s.dispatch("POST", "/w", body.as_bytes(), &mut out);
        }
        let per = t0.elapsed().as_secs_f64() / n as f64;
        report.add("write", per, 0);
    }
    {
        let t0 = Instant::now();
        let n = 1_000;
        let mut out = Vec::new();
        for _ in 0..n {
            out.clear();
            let body = b"{\"team\":\"x\",\"name\":\"d\"}";
            s.dispatch("POST", "/w", body, &mut out);
            let id = velo::value::parse_json(&out).unwrap().get("id").as_key();
            out.clear();
            s.dispatch("DELETE", &format!("/d/{id}"), b"", &mut out);
        }
        let per = t0.elapsed().as_secs_f64() / n as f64;
        report.add("create_delete", per, 0);
    }
    {
        let t0 = Instant::now();
        let n = 2_000;
        let mut out = Vec::new();
        for i in 0..n {
            out.clear();
            let body = format!("{{\"team\":\"x\",\"name\":\"w{i}\"}}");
            s.dispatch("POST", "/w", body.as_bytes(), &mut out);
            out.clear();
            s.dispatch("GET", "/u", b"", &mut out);
        }
        let per = t0.elapsed().as_secs_f64() / n as f64;
        report.add("write_then_list", per, out.len());
    }
    for (name, path) in [
        ("all", "/u"),
        ("where", "/w?t=rare"),
        ("order", "/o"),
        ("find", "/f"),
        ("chain", "/c?t=x"),
        ("select", "/s"),
    ] {
        let t0 = Instant::now();
        let n = 20_000;
        let mut out = Vec::new();
        for _ in 0..n {
            out.clear();
            s.dispatch("GET", path, b"", &mut out);
        }
        let per = t0.elapsed().as_secs_f64() / n as f64;
        report.add(name, per, out.len());
    }
    std::process::exit(report.finish());
}
