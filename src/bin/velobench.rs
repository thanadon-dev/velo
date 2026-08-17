use velo::bench::{run, Args};

fn seconds(text: &str) -> Option<std::time::Duration> {
    let n: f64 = text.parse().ok()?;
    (n > 0.0).then(|| std::time::Duration::from_secs_f64(n.clamp(0.01, 86_400.0)))
}

fn parse_args() -> Args {
    let mut a = Args::default();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let mut next = || {
            i += 1;
            argv.get(i).cloned().unwrap_or_default()
        };
        match arg {
            "-c" => a.conns = next().parse().unwrap_or(a.conns),
            "-d" => a.run_for = seconds(&next()).unwrap_or(a.run_for),
            "-p" => a.pipeline = next().parse().unwrap_or(a.pipeline).max(1),
            "-m" => a.method = next().to_uppercase(),
            "-b" => a.body = next(),
            "-H" => a.headers.push(next()),
            "-h" | "--help" => {
                eprintln!("usage: velobench [-c conns] [-d secs] [-p depth] [-m method] [-b body] <http://host:port/path | unix:/sock//path>");
                std::process::exit(0);
            }
            url if url.starts_with("unix:") => {
                let (sock, path) = url.split_once("//").unwrap_or((url, "/"));
                a.unix = Some(sock.trim_end_matches('/').to_string());
                a.path = format!("/{}", path.trim_start_matches('/'));
            }
            url => {
                let rest = url.strip_prefix("http://").unwrap_or(url);
                let (hostport, path) = match rest.find('/') {
                    Some(j) => (&rest[..j], &rest[j..]),
                    None => (rest, "/"),
                };
                let (h, p) = match hostport.rsplit_once(':') {
                    Some((h, p)) => (h, p.parse().unwrap_or(80)),
                    None => (hostport, 80),
                };
                a.host = h.to_string();
                a.port = p;
                a.path = path.to_string();
            }
        }
        i += 1;
    }
    a
}

fn main() {
    let a = parse_args();
    let (method, path, conns) = (a.method.clone(), a.path.clone(), a.conns);
    let where_ = a.unix.clone().unwrap_or_else(|| format!("{}:{}", a.host, a.port));
    let r = run(a);
    println!("{method} {path} on {where_} -> {conns} conns, {:.1}s", r.elapsed);
    println!("requests    {}", r.requests);
    println!("errors      {}", r.errors);
    println!("throughput  {:.0} req/s", r.per_second());
    println!("transfer    {:.1} MB/s", r.bytes as f64 / r.elapsed / 1e6);
    println!("latency     p50 {:.3} ms  p99 {:.3} ms  max {:.3} ms", r.p50, r.p99, r.max);
}
