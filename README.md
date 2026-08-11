# Velo

**v0.2.0** — a tiny language for HTTP APIs, written in Rust with zero dependencies. One line per endpoint, compiled to an expression tree, served by a hand-written HTTP/1.1 engine.

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

`ab -k -c 50 -n 30000`, 4-core box, release build (`lto = "fat"`, `codegen-units = 1`), v0.2.0:

| route | kind | req/s |
| --- | --- | --- |
| `/health` | const fold | 38 800 |
| `/users/1` | store lookup | 38 900 |
| `/stats` | store count | 39 400 |
| `/teams/:tid/members/:mid` | 2 params | 38 000 |

Resident memory while serving: **488 kB**. Binary: 594 kB, statically linked.

Numbers are load-generator bound (`ab` is single-threaded); reproduce with `./bench.sh`.

## Tests

```sh
cargo test
```

14 tests: const folding, CRUD, params, body fields, error codes, JSON round-trip and escaping, raw-socket HTTP (keep-alive, pipelining, HEAD, chunked rejection, split requests), concurrent writes.

## Build notes

`.cargo/config.toml` targets `x86_64-unknown-linux-musl` with `rust-lld`, so the build needs no system C toolchain. Remove that file to build against glibc with `cc`.

## Changelog

**v0.2.0** — rewritten in Rust, zero dependencies. Hand-written HTTP/1.1 engine (keep-alive, pipelining, HEAD fallback, chunked rejection), FNV router, `Arc`-backed values, copy-on-write store, const-folded routes, 488 kB RSS.

**v0.1.0** — first version (Go): language, closure compiler, router, in-memory store.
