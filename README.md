# Velo

**v0.10.0** — a tiny language for HTTP APIs, written in Rust with zero dependencies. One line per endpoint, compiled to an expression tree, served by an epoll event loop.

```velo
GET    /health     => "ok"
GET    /users      => db.users.all()
GET    /users/:id  => db.users.find(id)
POST   /users      => db.users.create(body)
PUT    /users/:id  => db.users.update(id, body)
DELETE /users/:id  => db.users.delete(id)
GET    /stats      => { users: db.users.count(), ok: true }
GET    /search     => db.users.where("team", query.team)
DELETE /users/:id  => db.users.delete(id) : 204
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
| `velo run <file> [addr] [--data f.json]` | start the server (default `:8080`, env `VELO_ADDR`, `VELO_DATA`) |
| `velo check <file>` | compile only, report errors with the offending source line |
| `velo routes <file>` | list compiled routes and which ones fold to constants |
| `velo version` | print version |
| `velobench [-c n] [-d secs] [-p depth] [-m method] [-b body] <url>` | built-in keep-alive load generator |

## Language

A program is a list of routes:

```
METHOD /path/:param => expression
METHOD /path        => expression : status
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
| query string | `query.limit` | parsed only if the route mentions it, percent-decoded |
| store call | `db.users.find(id)` | see below |
| function call | `now()`, `uuid()`, `len(x)`, `env("PORT")` | see below |

Built-in store (`db.<collection>.<op>`):

| op | returns | on miss |
| --- | --- | --- |
| `all()` | array of rows | |
| `count()` | number | |
| `find(key)` | row | 404 |
| `where(field, value)` | array of matching rows | `[]` |
| `page(offset, limit)` | slice of rows, `limit` 0 means "to the end" | `[]` |
| `order(field)` | rows sorted by `field`, `"-field"` for descending | `[]` |
| `create(value)` | row with generated `id` | 400 if body is empty |
| `update(key, patch)` | merged row | 404 |
| `delete(key)` | `{"deleted":true}` | 404 |

Built-in functions:

| function | returns |
| --- | --- |
| `now()` | Unix time in milliseconds |
| `uuid()` | random v4 UUID string |
| `len(x)` | length of a string, array, or object (`null` is 0) |
| `env("NAME")` | environment variable, or `null`; folded at compile time |

```velo
POST /events => db.events.create({ id: uuid(), at: now(), data: body })
```

`POST` routes answer `201`, everything else `200`. Append `: <code>` to set it yourself:

```velo
DELETE /users/:id => db.users.delete(id) : 204
POST   /jobs      => db.jobs.create(body) : 202
```

`204` and `304` are sent without a body or `Content-Length`. Errors are `{"error":"..."}`.

## Persistence

The store is in memory. Pass `--data file.json` (or set `VELO_DATA`) and velo loads that file at boot and writes it back whenever the data changed, at most once every `VELO_SAVE_MS` milliseconds (default 200):

```sh
velo run examples/api.velo :8080 --data data.json
```

Saves are atomic (write to `.tmp`, then rename) and skipped entirely when nothing was written, so a read-only workload never touches the disk. `SIGINT` and `SIGTERM` stop the event loop and write a final snapshot before exiting, so an orderly shutdown loses nothing; a hard kill can lose at most the last save interval.

## Design

- **Const folding.** Routes whose expression touches no param, body, or store are evaluated at compile time and stored as ready-to-send bytes. `GET /health => "ok"` costs one `memcpy` per request.
- **Router.** Per-method exact map (FNV-hashed) for static paths, a segment tree for `:param` paths. Params are borrowed slices of the request line, never copied.
- **Values.** `Value` is an enum with `Arc` payloads, so returning a whole collection is a refcount bump, not a deep copy. JSON is written straight into the connection's output buffer.
- **Rendered-once JSON.** Every stored row keeps its JSON bytes next to its fields, and each collection caches the JSON of its full row list; both are rebuilt only when the collection is written to. `GET /users` is then a `memcpy`, not a serialization pass. The cost is holding rows twice in memory.
- **Store.** Copy-on-write snapshot behind an `RwLock`; readers clone an `Arc<Vec<Value>>` and release the lock immediately.
- **HTTP.** Hand-written HTTP/1.1: keep-alive by default, request pipelining, per-connection read/write/body buffers reused across requests, batched writes.
- **Event loop.** One epoll instance per worker thread (default: one per core), all sharing the listener with `EPOLLEXCLUSIVE`. Connections are non-blocking and cost a few kB each instead of a thread and a stack: 1 000 live connections fit in under 1 MB of RSS. `epoll` is called through three `extern "C"` declarations, still no crates.
- **No dependencies.** `[dependencies]` is empty. std only.

Env knobs:

| variable | default | effect |
| --- | --- | --- |
| `VELO_ADDR` | `:8080` | listen address |
| `VELO_WORKERS` | cores | event loops, one thread each |
| `VELO_MAX_CONNS` | 65536 | live connections per worker, extra ones get 503 |
| `VELO_KEEPALIVE` | 60 | idle seconds before a connection is swept |
| `VELO_DATA` | off | snapshot file, same as `--data` |
| `VELO_SAVE_MS` | 200 | minimum gap between snapshots |
| `VELO_CORS` | off | value for `Access-Control-Allow-Origin`; also answers `OPTIONS` preflight with 204 |
| `VELO_LOG` | off | one line per request on stderr; costs about 75% of throughput, so keep it for development |

## Benchmarks

Load generator: `velobench` (ships in this repo, thread per connection, keep-alive). 4-core box, client and server share the machine, release build, v0.10.0:

| route | kind | req/s | p50 | p99 |
| --- | --- | --- | --- | --- |
| `/health` | const fold | 96 500 | 0.44 ms | 2.19 ms |
| `/users` (200 rows, 9 kB) | cached list | 53 900 | | |
| `/users/:id` | store lookup | 84 800 | 0.49 ms | 2.31 ms |
| `/users/page` (20 of 200) | slice + render | 56 700 | | |
| `/stats` | 2 store counts | 70 500 | 0.37 ms | 6.07 ms |
| `/teams/:tid/members/:mid` | 2 params | 66 400 | 0.44 ms | 5.39 ms |
| `POST /users` | JSON parse + insert | 62 200 | 0.63 ms | 3.33 ms |

`-c 50` unless noted. With pipelining (`-p 16`): **1 027 000 req/s** on `/health`.

Connection scaling (`/health`, then `/users/:id`), server RSS measured while serving:

| conns | req/s | p50 | RSS |
| --- | --- | --- | --- |
| 50 | 90 500 | 0.41 ms | 644 kB |
| 500 | 78 000 | 2.77 ms | 764 kB |
| 1 000 | 68 700 | 10.8 ms | 896 kB |

The load generator (one thread per connection) is the bottleneck at high connection counts, not the server. Binary: 594 kB, statically linked.

Reproduce:

```sh
./bench.sh
velobench -c 200 -d 10 -p 16 http://127.0.0.1:8099/users/1
```

## Tests

```sh
cargo test
```

29 tests (28 integration + 1 in velobench): const folding, CRUD, params, body fields, error codes, JSON round-trip and escaping, query params, percent-decoding, `where` filters, persistence round-trip, status overrides, paging, list-cache invalidation, graceful shutdown, built-ins, CORS preflight, sorting, compile-error formatting, raw-socket HTTP (keep-alive, pipelining, HEAD, chunked rejection, split requests, 100 concurrent connections), concurrent writes.

## Build notes

`.cargo/config.toml` targets `x86_64-unknown-linux-musl` with `rust-lld`, so the build needs no system C toolchain. Remove that file to build against glibc with `cc`.

## Changelog

**v0.10.0** — `db.x.order(field)` sorting (`"-field"` descending, numbers compare numerically) and compile errors that print the offending source line.

**v0.9.0** — `VELO_CORS` adds the allow-origin headers and answers `OPTIONS` preflight with a bodyless 204; `VELO_LOG` prints one line per request.

**v0.8.0** — built-in functions: `now()`, `uuid()` (v4, seeded from `/dev/urandom`, per-thread xorshift after that), `len(x)`, `env("NAME")`.

**v0.7.1** — graceful shutdown: `SIGINT`/`SIGTERM` unwind the event loop and flush a final snapshot when `--data` is set. `Server::shutdown()` does the same from code.

**v0.7.0** — rows carry their rendered JSON and collections cache the JSON of the whole list, invalidated on write. Paged reads 47.4k to 56.7k req/s, list reads become a memcpy.

**v0.6.0** — per-route status override (`expr : 204`, bodyless 204/304 responses) and `db.x.page(offset, limit)` for pagination.

**v0.5.0** — optional persistence: `--data file.json` loads at boot and autosaves on change (atomic rename, dirty-flag gated, `VELO_SAVE_MS`). POST throughput 49.5k to 62.2k req/s on the epoll loop.

**v0.4.0** — query strings (`query.name`), percent-decoding for path params and query values, `db.x.where(field, value)` filters. Query parsing happens only for routes that mention `query`.

**v0.3.0** — epoll event loop replaces thread-per-connection: one epoll per core sharing the listener with `EPOLLEXCLUSIVE`, non-blocking sockets, EPOLLOUT-driven partial-write handling, idle sweep on `VELO_KEEPALIVE`. 1 000 connections in 896 kB RSS, p99 on `/health` down from 3.96 ms to 2.25 ms.

**v0.2.1** — `velobench` load generator (thread per connection, pipelining, p50/p99), explicit `Connection: keep-alive` response header so standard tools reuse connections.

**v0.2.0** — rewritten in Rust, zero dependencies. Hand-written HTTP/1.1 engine (keep-alive, pipelining, HEAD fallback, chunked rejection), FNV router, `Arc`-backed values, copy-on-write store, const-folded routes, 488 kB RSS.

**v0.1.0** — first version (Go): language, closure compiler, router, in-memory store.
