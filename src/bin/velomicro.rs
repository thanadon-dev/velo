use std::time::Instant;
use velo::{compile, Server};

fn main() {
    let rows: usize = std::env::args().nth(1).and_then(|v| v.parse().ok()).unwrap_or(500);
    let src = "GET /u => db.users.all()\n\
               GET /w => db.users.where(\"team\", query.t)\n\
               GET /o => db.users.order(\"name\")\n\
               GET /f => db.users.find(\"1\")\n\
               POST /w => db.users.create(body)\n";
    let store = velo::Store::new();
    let prog = compile(src, Some(store.clone())).unwrap();
    let s = Server::new(prog).unwrap();
    let col = store.collection("users");
    for i in 0..rows {
        let v = velo::value::parse_json(format!("{{\"team\":\"x\",\"name\":\"u{i}\"}}").as_bytes())
            .unwrap();
        col.create(v).unwrap();
    }
    println!("{rows} rows");
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
        println!("{:8} {:>9.2} us/op  {:>10.0} op/s", "write", per * 1e6, 1.0 / per);
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
        println!(
            "{:8} {:>9.2} us/op  {:>10.0} op/s  {} bytes",
            "write+all",
            per * 1e6,
            1.0 / per,
            out.len()
        );
    }
    for (name, path) in [("all", "/u"), ("where", "/w?t=rare"), ("order", "/o"), ("find", "/f")] {
        let t0 = Instant::now();
        let n = 20_000;
        let mut out = Vec::new();
        for _ in 0..n {
            out.clear();
            s.dispatch("GET", path, b"", &mut out);
        }
        let per = t0.elapsed().as_secs_f64() / n as f64;
        println!(
            "{name:8} {:>9.2} us/op  {:>10.0} op/s  {} bytes",
            per * 1e6,
            1.0 / per,
            out.len()
        );
    }
}
