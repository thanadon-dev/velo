# Velo

**v0.2.1** — a tiny language for HTTP APIs, written in Rust with zero dependencies. One line per endpoint, compiled to an expression tree, served by a hand-written HTTP/1.1 engine.

```velo
GET    /health     => "ok"
GET    /users      => db.users.all()
GET    /users/:id  => db.users.find(id)
POST   /users      => db.users.create(body)
PUT    /users/:id  => db.users.update(id, body)
DELETE /users/:id  => db.users.delete(id)
GET    /stats      => { users: db.users.count(), ok: true }
```

That file is a complete, running API server.

## Quick start

```sh
cargo build --release
./target/x86_64-unknown-linux-musl/release/velo run examples/api.velo :8080

curl localhost:8080/health
curl -XPOST localhost:8080/users -d '{"name":"mark"}'
curl localhost:8080/users/1
```

CLI:

| command | what it does |
| --- | --- |
| `velo run <file> [addr]` | start the server (default `:8080`, env `VELO_ADDR`) |
| `velo check <file>` | compile only, report errors with line numbers |
| `velo routes <file>` | list compiled routes and which ones fold to constants |
| `velo version` | print version |
| `velobench [-c n] [-d secs] [-p depth] [-m method] [-b body] <url>` | built-in keep-alive load generator |

## Language

A program is a list of routes:

```
METHOD /path/:param => expression
```

Methods: `GET POST PUT PATCH DELETE HEAD OPTIONS`. Comments: `#` or `//`. `HEAD` falls back to the matching `GET` route.

Expressions:

| form | example | notes |
| --- | --- | --- |
| string | `"ok"` | served as `text/plain` at the top level |
| number, bool, null | `42`, `true`, `null` | |
| object | `{ status: "ok", n: db.users.count() }` | key order preserved |
| array | `[1, 2, 3]` | |
| path param | `id` | resolved to a slot index at compile time |
| request body | `body`, `body.name` | parsed only if the route mentions it |
| store call | `db.users.find(id)` | see below |

Built-in store (`db.<collection>.<op>`):

| op | returns | on miss |
| --- | --- | --- |
| `all()` | array of rows | |
| `count()` | number | |
| `find(key)` | row | 404 |
| `create(value)` | row with generated `id` | 400 if body is empty |
| `update(key, patch)` | merged row | 404 |
| `delete(key)` | `{"deleted":true}` | 404 |

`POST` routes answer `201`, everything else `200`. Errors are `{"error":"..."}`.

## Design

- **Const folding.** Routes whose expression touches no param, body, or store are evaluated at compile time and stored as ready-to-send bytes. `GET /health => "ok"` costs one `memcpy` per request.
- **Router.** Per-method exact map (FNV-hashed) for static paths, a segment tree for `:param` paths. Params are borrowed slices of the request line, never copied.
- **Values.** `Value` is an enum with `Arc` payloads, so returning a whole collection is a refcount bump, not a deep copy. JSON is written straight into the connection's output buffer.
- **Store.** Copy-on-write snapshot behind an `RwLock`; readers clone an `Arc<Vec<Value>>` and release the lock immediately.
- **HTTP.** Hand-written HTTP/1.1: keep-alive by default, request pipelining, per-connection read/write/body buffers reused across requests, one `write_all` per batch. Thread per connection with a 128 KB stack, capped by `VELO_MAX_CONNS`.
- **No dependencies.** `[dependencies]` is empty. std only.

Env knobs: `VELO_ADDR`, `VELO_MAX_CONNS` (default 4096), `VELO_KEEPALIVE` seconds (default 60).

## Benchmarks

Load generator: `velobench` (ships in this repo, thread per connection, keep-alive). 4-core box, client and server share the machine, release build, v0.2.1, `-c 50`:

| route | kind | req/s | p50 | p99 |
| --- | --- | --- | --- | --- |
| `/health` | const fold | 84 000 | 0.42 ms | 3.96 ms |
| `/version` | const fold, object | 76 500 | 0.49 ms | 3.75 ms |
| `/users` | store scan | 80 300 | 0.44 ms | 4.05 ms |
| `/users/:id` | store lookup | 73 800 | 0.49 ms | 4.23 ms |
| `/stats` | 2 store counts | 70 500 | 0.37 ms | 6.07 ms |
| `/teams/:tid/members/:mid` | 2 params | 66 400 | 0.44 ms | 5.39 ms |
| `POST /users` | JSON parse + insert | 49 500 | | |

With pipelining (`-p 16`): **1 040 000 req/s** on `/health`.

Resident memory: **488 kB** idle, ~7 MB with 50 live connections. Binary: 594 kB, statically linked.

Reproduce:

```sh
./bench.sh
velobench -c 200 -d 10 -p 16 http://127.0.0.1:8099/users/1
```

## Tests

```sh
cargo test
```

15 tests (14 integration + 1 in velobench): const folding, CRUD, params, body fields, error codes, JSON round-trip and escaping, raw-socket HTTP (keep-alive, pipelining, HEAD, chunked rejection, split requests), concurrent writes.

## Build notes

`.cargo/config.toml` targets `x86_64-unknown-linux-musl` with `rust-lld`, so the build needs no system C toolchain. Remove that file to build against glibc with `cc`.

## Changelog

**v0.2.1** — `velobench` load generator (thread per connection, pipelining, p50/p99), explicit `Connection: keep-alive` response header so standard tools reuse connections.

**v0.2.0** — rewritten in Rust, zero dependencies. Hand-written HTTP/1.1 engine (keep-alive, pipelining, HEAD fallback, chunked rejection), FNV router, `Arc`-backed values, copy-on-write store, const-folded routes, 488 kB RSS.

**v0.1.0** — first version (Go): language, closure compiler, router, in-memory store.
