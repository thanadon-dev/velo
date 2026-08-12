# Velo

**v0.21.2** — a tiny language for HTTP APIs, written in Rust with zero dependencies. One line per endpoint, compiled to an expression tree, served by an epoll event loop.

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
| `velo routes <file>` | list compiled routes, their kind, status, and guard |
| `velo openapi <file>` | print an OpenAPI 3.0 document for the routes |
| `velo new <file>` | write a starter file |
| `velo version` | print version |
| `velobench [-c n] [-d secs] [-p depth] [-m method] [-b body] <url>` | built-in keep-alive load generator |
| `velomicro [rows]` | microbenchmark of the dispatch path, no sockets |

## Language

A program is a list of routes:

```
METHOD /path/:param => expression
METHOD /path        => expression : status
METHOD /path        => expression when condition
METHOD /path        => expression when condition or status
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
| request header | `header.x_team` | lowercased, `-` written as `_`, parsed only if the route mentions it |
| store call | `db.users.find(id)` | see below |
| function call | `now()`, `uuid()`, `len(x)`, `env("PORT")` | see below |

Built-in store (`db.<collection>.<op>`):

| op | returns | on miss |
| --- | --- | --- |
| `all()` | array of rows | |
| `count()` | number | |
| `find(key)` | row | 404 |
| `first(field, value)` | first matching row | 404 |
| `where(field, value)` | array of matching rows; linear scan, cached per field and value | `[]` |
| `page(offset, limit)` | slice of rows, `limit` 0 means "to the end" | `[]` |
| `order(field)` | rows sorted by `field`, `"-field"` for descending | `[]` |
| `create(value)` | the stored row; `id` is generated unless the value carries one | 400 on an empty body, 409 on a duplicate `id` |
| `update(key, patch)` | merged row | 404 |
| `delete(key)` | `{"deleted":true}` | 404 |

Built-in functions:

| function | returns |
| --- | --- |
| `now()` | Unix time in milliseconds |
| `uuid()` | random v4 UUID string |
| `len(x)` | length of a string, array, or object (`null` is 0) |
| `env("NAME")` | environment variable, or `null`; folded at compile time |
| `openapi()` | this API's OpenAPI 3.0 document, rendered once at compile time |

```velo
POST /events     => db.events.create({ id: uuid(), at: now(), data: body })
GET  /users/mine => db.users.where("team", header.x_team)
```

`POST` routes answer `201`, everything else `200`. Append `: <code>` to set it yourself:

```velo
DELETE /users/:id => db.users.delete(id) : 204
POST   /jobs      => db.jobs.create(body) : 202
```

`204` and `304` are sent without a body or `Content-Length`. Errors are `{"error":"..."}`.

## Guards

`when <condition>` runs before the route body. A falsy condition answers `401 {"error":"unauthorized"}` and the body never runs:

```velo
GET    /admin/users => db.users.all() when header.authorization == env("ADMIN_TOKEN")
GET    /mine        => db.users.where("team", header.x_team) when header.x_team
DELETE /users/:id   => db.users.delete(id) : 204 when header.x_key != "readonly"
```

A condition is one expression, optionally compared with `==` or `!=`. On its own an expression is truthy unless it is `null`, `false`, `0`, or an empty string. Guarded routes are never const-folded.

`or <status>` changes what a failed guard answers, which turns a guard into input validation:

```velo
POST /users => db.users.create(body) when body.name or 400
GET  /root  => db.audit.all() when header.x_key == env("ROOT_KEY") or 403
```

`examples/todo.velo` is a complete todo API using uuid keys, timestamps, sorting, and filters.

## OpenAPI

`velo openapi app.velo` prints an OpenAPI 3.0 document built from the compiled routes: paths with `{param}` placeholders, path/query/header parameters taken from what each route actually reads, request bodies for routes that use `body`, and the status codes each route can answer including guard failures.

```sh
velo openapi examples/todo.velo > openapi.json
```

The same document can be served by the API itself. `openapi()` is folded to constant bytes at compile time, so the route costs a `memcpy` like any other constant (84k req/s in the same benchmark as `/health`):

```velo
GET /openapi.json => openapi()
```

`VELO_TITLE` sets the document title.

## Persistence

The store is in memory. Pass `--data file.json` (or set `VELO_DATA`) and velo loads that file at boot and writes it back whenever the data changed, at most once every `VELO_SAVE_MS` milliseconds (default 200):

```sh
velo run examples/api.velo :8080 --data data.json
```

Saves are atomic (write to `.tmp`, then rename) and skipped entirely when nothing was written, so a read-only workload never touches the disk. The gap between snapshots adapts: velo measures how long the last save took and waits until that save has cost at most `VELO_SAVE_DUTY` percent of wall time, so a 5 MB dataset under sustained writes is not rewritten five times a second. `SIGINT` and `SIGTERM` stop the event loop and write a final snapshot before exiting, so an orderly shutdown loses nothing; a hard kill can lose at most the last save interval.

## Layout

| file | what it holds |
| --- | --- |
| `src/lexer.rs` | tokens |
| `src/parser.rs` | source to routes, const folding, error messages |
| `src/ast.rs` | `Expr` tree, evaluation, built-in functions, request context |
| `src/store.rs` | collections, snapshots, JSON caches, persistence |
| `src/router.rs` | per-method exact map and param tree |
| `src/http.rs` | `Server`, dispatch, metrics, status codes |
| `src/serve.rs` | request parsing, connection state, the epoll loop |
| `src/value.rs` | `Value`, JSON reader and writer |
| `src/date.rs` | `Date` header formatting |
| `src/openapi.rs` | OpenAPI 3.0 document generation |
| `src/main.rs` | CLI |
| `src/bin/velobench.rs` | load generator |
| `src/bin/velomicro.rs` | in-process dispatch microbenchmark, `velomicro [rows]` |

## Design

- **Const folding.** Routes whose expression touches no param, body, or store are evaluated at compile time and stored as ready-to-send bytes. `GET /health => "ok"` costs one `memcpy` per request.
- **Router.** Per-method exact map (FNV-hashed) for static paths, a segment tree for `:param` paths. Params are borrowed slices of the request line, never copied.
- **Values.** `Value` is an enum with `Arc` payloads, so returning a whole collection is a refcount bump, not a deep copy. JSON is written straight into the connection's output buffer.
- **Rendered-once JSON.** Every stored row keeps its JSON bytes next to its fields, and each collection caches the JSON of its full row list and up to 32 sort orders and filters, inside a byte budget; all of it is rebuilt only when the collection is written to. `GET /users` and `order(...)` are then a `memcpy`, not a sort and a serialization pass. The cost is holding rows twice in memory.
- **No allocation for key lookups.** `db.users.find(id)` on a plain path param hashes the slice of the request line directly; nothing is copied unless the param is percent-encoded.
- **Store.** Copy-on-write snapshot behind an `RwLock`; readers clone an `Arc<Vec<Value>>` and release the lock immediately.
- **HTTP.** Hand-written HTTP/1.1: keep-alive by default, request pipelining, per-connection read/write/body buffers reused across requests, batched writes. A connection stops rendering further pipelined requests once 256 kB of response bytes are waiting, so a client cannot make the server buffer an unbounded amount by pipelining requests for large lists; it resumes as soon as the socket drains. `Date` is formatted once per second per worker, not per response. `Expect: 100-continue` gets its interim response as soon as the headers arrive. Chunked bodies are refused with 411, conflicting `Content-Length` headers with 400, oversized bodies with 413, oversized headers with 431.
- **Event loop.** One epoll instance per worker thread (default: one per core), all sharing the listener with `EPOLLEXCLUSIVE`. Connections are non-blocking and cost a few kB each instead of a thread and a stack: 1 000 live connections fit in under 1 MB of RSS. `epoll` is called through three `extern "C"` declarations, still no crates.
- **No dependencies.** `[dependencies]` is empty. std only.

Env knobs:

| variable | default | effect |
| --- | --- | --- |
| `VELO_ADDR` | `:8080` | listen address |
| `VELO_WORKERS` | cores | event loops, one thread each |
| `VELO_MAX_CONNS` | 65536 | live connections per worker, extra ones get 503 |
| `VELO_KEEPALIVE` | 60 | idle seconds before a served connection is swept |
| `VELO_HEADER_TIMEOUT` | 10 | seconds a connection may spend before its first complete request; drip-feeding headers does not extend it |
| `VELO_DATA` | off | snapshot file, same as `--data` |
| `VELO_SAVE_MS` | 200 | minimum gap between snapshots |
| `VELO_SAVE_DUTY` | 10 | percent of wall time a snapshot may cost; the gap grows with the file so big datasets are not rewritten every 200 ms |
| `VELO_CORS` | off | value for `Access-Control-Allow-Origin`; also answers `OPTIONS` preflight with 204 |
| `VELO_LOG` | off | one line per request on stderr; costs about 75% of throughput, so keep it for development |
| `VELO_METRICS` | off | path that answers a metrics JSON, e.g. `/_metrics` |
| `VELO_TITLE` | `velo api` | title used by `openapi()` |
| `VELO_CACHE_BYTES` | 8 MB | budget for rendered list, sort, and filter caches; exceeding it clears them |
| `VELO_RATE` | off | requests per second allowed per client; over it answers 429 |
| `VELO_REAL_IP_HEADER` | off | header holding the client IP behind a proxy, e.g. `CF-Connecting-IP`; without it the socket address is used |
| `VELO_ETAG` | off | send `ETag` on 200 `GET`/`HEAD` responses and answer 304 to a matching `If-None-Match` |

## Benchmarks

Load generator: `velobench` (ships in this repo, thread per connection, keep-alive). 4-core box, client and server share the machine, release build, v0.21.2. The `users` collection holds 501 rows (16 kB as JSON). The `users` collection holds 200 rows.

`-c 50`, one request in flight per connection — client-bound, both processes fight for the same 4 cores:

| route | kind | req/s |
| --- | --- | --- |
| `/health` | const fold | 88 800 |
| `/users/:id` | store lookup | 88 000 |
| `/stats` | 2 store counts | 80 700 |
| `/users` (501 rows, 16 kB) | cached list | 74 300 |
| `/users/by/team` | cached filter | 68 300 |
| `/users/sorted` | cached sort | 60 700 |
| `/users/page` (20 of 501) | slice + render | 54 200 |
| `POST /users` | JSON parse + insert | 53 500 |

`-c 8 -p 32`, pipelined — what the server itself can do:

| route | req/s | transfer |
| --- | --- | --- |
| `/health` | 862 000 | 122 MB/s |
| `/users/:id` | 786 000 | 127 MB/s |
| `/stats` | 417 000 | 65 MB/s |
| `/users` (16 kB each) | 304 000 | 5.0 GB/s |
| `/users/sorted` | 139 000 | 2.3 GB/s |
| `/users/by/team` | 97 000 | 16 MB/s |

In-process, no sockets (`velomicro 5000`): `find` 0.23 us, `where` 1.19 us, `all` 5.8 us, `order` 6.4 us per call.

Run read benchmarks before write benchmarks, or restart in between: a `POST` run at 50k req/s adds a hundred thousand rows and every later list measurement is then measuring a much bigger response.

Connection scaling (`/health`), server RSS while serving:

| conns | req/s | p50 | RSS |
| --- | --- | --- | --- |
| 50 | 90 500 | 0.41 ms | 644 kB |
| 500 | 78 000 | 2.77 ms | 764 kB |
| 1 000 | 68 700 | 10.8 ms | 896 kB |

Binary: 594 kB, statically linked.

Reproduce:

```sh
./bench.sh
velobench -c 8 -p 32 -d 5 http://127.0.0.1:8099/users/1
```

## Tests

```sh
cargo test
```

60 tests (47 integration + 5 CLI + 5 fuzz + 3 unit): const folding, CRUD, params, body fields, error codes, JSON round-trip and escaping, query params, percent-decoding, `where` filters, persistence round-trip, status overrides, paging, list-cache invalidation, graceful shutdown, built-ins, CORS preflight, sorting, compile-error formatting, `Date` formatting, header hardening, sort-cache and filter-cache invalidation, request headers, guards, client-supplied ids, metrics, ETag round-trip, rate limiting, raw-socket HTTP (keep-alive, pipelining, HEAD, chunked rejection, split requests, 100 concurrent connections), concurrent writes.

`tests/cli.rs` drives the built binary end to end: `check` exit codes and error text, `new` refusing to overwrite, `openapi` output parsed back as JSON, a metrics endpoint, and a `POST` surviving a `SIGTERM` restart through the snapshot file.

`tests/fuzz.rs` adds four deterministic robustness tests: 2 000 mutated sources and 2 000 random byte strings through the compiler, 300 connections of malformed and truncated HTTP, and oversized header and body requests. They also cover slow drip-feeding clients. They assert the process never panics and that the server still answers a normal request afterwards.

## Rate limiting

`VELO_RATE=100` allows each client 100 requests per second and answers `429 Too Many Requests` beyond that, counted in one-second windows across 16 shards. Behind a proxy every socket looks like `127.0.0.1`, so set `VELO_REAL_IP_HEADER=CF-Connecting-IP` (or `X-Forwarded-For`) to key on the real client — only trust that header if a proxy you control sets it.

The check costs a lock and a hash per request. In a loopback benchmark, where every request shares one key and all workers contend on the same shard, `/health` drops from 90.6k to 71.9k req/s; real traffic spread over many client IPs spreads over the shards.

## Metrics

Set `VELO_METRICS=/_metrics` and that path answers:

```json
{"version":"0.16.0","uptime_ms":3747,"requests":275021,"failures":1,"connections":1,"routes":21,"workers":4}
```

`failures` counts responses velo generated itself (404, 405, 400, 401, 409, 413, and store misses), `connections` is the live count across workers. The counters are relaxed atomics; the endpoint measured no throughput cost. Point a monitor at it, or at any route in your API.

## Deployment

Velo is one static binary and one text file. A systemd user unit is enough:

```ini
[Unit]
Description=velo api
After=network.target

[Service]
ExecStart=/usr/local/bin/velo run /srv/api/app.velo 127.0.0.1:8080 --data /srv/api/data.json
Environment=VELO_WORKERS=4
Restart=always
KillSignal=SIGTERM

[Install]
WantedBy=default.target
```

`SIGTERM` is the clean stop: the event loop unwinds and the final snapshot is written before exit. Put a TLS terminator in front of it; velo speaks plain HTTP/1.1 only.

## CI

`./check.sh` is the gate: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, the full test suite, a release build, both examples compiled, and a boot smoke test with a short benchmark. `.github/workflows/ci.yml` runs exactly the same steps on push.

## Build notes

`.cargo/config.toml` targets `x86_64-unknown-linux-musl` with `rust-lld`, so the build needs no system C toolchain. Remove that file to build against glibc with `cc`.

## Changelog

**v0.21.2** — bounded response buffering: pipelined requests stop being rendered past 256 kB of pending output and resume after the flush. 100 pipelined 16 kB responses now cost 1.1 MB of RSS instead of growing with the batch, at the same throughput.

**v0.21.1** — end-to-end CLI tests: the real binary is started, driven over TCP, stopped with `SIGTERM`/`SIGINT`, and restarted to prove the snapshot round-trips.

**v0.21.0** — `openapi()` built-in: a route can serve this API's own OpenAPI document, folded to constant bytes at compile time.

**v0.20.0** — per-client rate limiting: `VELO_RATE` requests per second, keyed on the socket address or on `VELO_REAL_IP_HEADER` behind a proxy, answered with 429.

**v0.19.1** — snapshot interval is now self-tuning: a save may cost at most `VELO_SAVE_DUTY` percent of wall time (10 by default). Under a sustained 45k writes/s load on a 5 MB dataset that cut disk writes from 53 MB to 42 MB per 5 s while running slightly faster.

**v0.19.0** — `velo openapi` generates an OpenAPI 3.0 document from the compiled routes. The compiler now records which query and header fields each route reads, so the document lists real parameters instead of guesses.

**v0.18.2** — the render caches now respect a byte budget (`VELO_CACHE_BYTES`, 8 MB by default) instead of only an entry count, so 32 large lists cannot quietly hold hundreds of megabytes.

**v0.18.1** — `where` results are cached per field and value alongside the sorted and full-list caches, all cleared on any write (`where` 77 us to 1.2 us per call on 5 000 rows). `velobench` now parses `Content-Length` instead of scanning every byte, so large responses measure the server rather than the client.

**v0.18.0** — `when <condition> or <status>` picks the status a failed guard answers, so a guard doubles as body validation (`when body.name or 400`). `velo routes` now prints each route guard.

**v0.17.1** — `where` and `first` compare fields without allocating a string per row and `order` extracts each sort key once: `where` over HTTP went from 2.4k to 82k req/s on a 500-row collection. Added `velomicro`, an in-process dispatch benchmark.

**v0.17.0** — optional `ETag` / `If-None-Match` (`VELO_ETAG=1`): 200 `GET` and `HEAD` responses carry an FNV tag of the body and a matching conditional request answers 304 without the body.

**v0.16.2** — `Expect: 100-continue` is answered with an interim `100 Continue` instead of leaving the client to time out before sending its body.

**v0.16.1** — split the two large modules: evaluation moved out of `parser.rs` into `ast.rs`, the event loop out of `http.rs` into `serve.rs`. Auto-generated ids now skip over ids a client already claimed instead of returning 409.

**v0.16.0** — optional metrics endpoint (`VELO_METRICS=/_metrics`) reporting version, uptime, requests, failures, live connections, routes, and workers.

**v0.15.1** — connections that never complete a request are dropped after `VELO_HEADER_TIMEOUT` (10s) measured from accept, so drip-feeding headers cannot hold a slot; `check.sh` mirrors CI locally.

**v0.15.0** — `db.x.first(field, value)`, `create` honours an `id` supplied in the body (409 on duplicates), `velo new` writes a starter file, `examples/todo.velo`, and deployment notes.

**v0.14.1** — repository hygiene: `rustfmt.toml`, formatted tree, zero clippy warnings, and a GitHub Actions workflow running fmt, clippy, tests, release build, and a boot smoke test.

**v0.14.0** — route guards: `when <condition>` with `==` / `!=` or a truthiness check, answering 401 before the body runs.

**v0.13.0** — request headers in expressions: `header.x_team` (lowercased, hyphens as underscores), parsed only for routes that mention `header`.

**v0.12.1** — deterministic fuzz suite for the compiler and the HTTP parser. No panics or hangs found; kept as a regression net.

**v0.12.0** — `order(...)` results are cached per sort key and invalidated on write (6k to 120k req/s pipelined), and `find`/`update`/`delete` on a plain path param no longer allocate a key (`/users/:id` 562k to 1 037k req/s pipelined).

**v0.11.0** — spec and hardening pass: `Date` response header (computed once per second per worker), 400 on conflicting `Content-Length` headers, backoff instead of a spin loop when `accept` fails with no file descriptors left.

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
