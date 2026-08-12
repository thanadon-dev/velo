# Velo

**v0.41.1** — a tiny language for HTTP APIs, written in Rust with zero dependencies. One line per endpoint, compiled to an expression tree, served by an epoll event loop.

```velo
GET    /health     => "ok"
GET    /users      => db.users.all()
GET    /users/:id  => db.users.find(id)
POST   /users      => db.users.create(body) when body.name else 400
PUT    /users/:id  => db.users.update(id, body)
DELETE /users/:id  => db.users.delete(id) : 204
GET    /search     => db.users.where("team", query.team)
GET    /stats      => { users: db.users.count(), avg: db.users.avg("score") }
```

That file is a complete, running API server.

Linux only: the event loop is epoll, and the build stops with a clear message anywhere else.

[Quick start](#quick-start) · [Language](#language) · [Guards](#guards) · [OpenAPI](#openapi) · [Persistence](#persistence) · [Rate limiting](#rate-limiting) · [Metrics](#metrics) · [Deployment](#deployment) · [Design](#design) · [Benchmarks](#benchmarks) · [Tests](#tests) · [CI](#ci) · [Layout](#layout) · [Build notes](#build-notes) · [Changelog](#changelog)

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
| `velo run <file> [addr] [--data f.json] [--watch]` | start the server; `addr` is `:8080`, `127.0.0.1:8080`, or `unix:/run/velo.sock` |
| `velo check <file>` | compile only, report errors with the offending line, column, and a caret |
| `velo routes <file>` | list compiled routes with their kind, status, guard, and source file and line |
| `velo openapi <file>` | print an OpenAPI 3.0 document for the routes |
| `velo new <file>` | write a starter file |
| `velo version` | print version |
| `velobench [-c n] [-d secs] [-p depth] [-m method] [-b body] <url>` | built-in keep-alive load generator; `unix:/sock//path` targets a Unix socket |
| `velomicro [rows] [--json] [--check <baseline>]` | microbenchmark of the dispatch path; `--check` fails when an operation is far slower than `bench/baseline.json` |

## Language

A program is a list of routes:

```
METHOD /path/:param => expression
METHOD /path        => expression : status
METHOD /path        => expression when condition
METHOD /path        => expression when condition else status
```

Methods: `GET POST PUT PATCH DELETE HEAD OPTIONS`. Comments: `#` or `//`. `HEAD` falls back to the matching `GET` route.

A file can pull in others, resolved relative to the file that names them. A file already loaded is skipped, so cycles are harmless:

```velo
include "parts/users.velo"
include "parts/posts.velo"

GET /health => "ok"
```

Expressions:

| form | example | notes |
| --- | --- | --- |
| string | `"ok"` | served as `text/plain` at the top level |
| number, bool, null | `42`, `true`, `null` | |
| object | `{ status: "ok", n: db.users.count() }` | key order preserved |
| array | `[1, 2, 3]` | |
| path param | `id` | resolved to a slot index at compile time |
| request body | `body`, `body.name` | parsed only if the route mentions it; JSON, or form-encoded as a fallback |
| query string | `query.limit` | parsed only if the route mentions it, percent-decoded |
| request header | `header.x_team` | lowercased, `-` written as `_`, parsed only if the route mentions it |
| store call | `db.users.find(id)` | see below |
| function call | `now()`, `uuid()`, `len(x)`, `env("PORT")` | see below |
| arithmetic | `query.page + 1`, `body.price * body.qty`, `(a + b) * 2` | `+` on two strings concatenates |
| comparison | `query.limit < 100`, `header.x_key == env("KEY")` | numeric when both sides parse as numbers, otherwise string-to-string, otherwise false |

Built-in store (`db.<collection>.<op>`):

| op | returns | on miss |
| --- | --- | --- |
| `all()` | array of rows | |
| `count()` | number |
| `sum(field)` | sum of numeric values in `field`, `0` when there are none |
| `avg(field)` | mean of numeric values, `null` when there are none |
| `min(field)` / `max(field)` | smallest / largest numeric value, `null` when there are none | |
| `find(key)` | row | 404 |
| `first(field, value)` | first matching row | 404 |
| `where(field, value)` | array of matching rows; linear scan, cached per field and value | `[]` |
| `page(offset, limit)` | slice of rows, `limit` 0 means "to the end" | `[]` |
| `search(field, text)` | rows whose `field` contains `text`, case-insensitive | `[]` |
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
| `file("page.html")` | the file's contents, read at compile time, served with a content type from its extension |

```velo
GET  /scores     => { total: db.orders.sum("amount"), avg: db.orders.avg("amount") }
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

A condition is any expression, usually a comparison, and conditions combine with `and` / `or` (short-circuiting). On its own an expression is truthy unless it is `null`, `false`, `0`, or an empty string. Guarded routes are never const-folded.

`else <status>` changes what a failed guard answers, which turns a guard into input validation:

```velo
POST /users      => db.users.create(body) when body.name else 400
GET  /root       => db.audit.all() when header.x_key == env("ROOT_KEY") else 403
GET  /users/page => db.users.page(query.offset, query.limit) when query.limit < 200 else 400
GET  /mine       => db.orders.where("customer", header.x_customer) when header.x_customer and header.x_key else 400
```

`examples/todo.velo` is a complete todo API using uuid keys, timestamps, sorting, and filters. `examples/shop/` splits a larger API over four files with `include`: a catalog with search, orders keyed by a customer header, and an admin section behind a token guard.

## OpenAPI

`velo openapi app.velo` prints an OpenAPI 3.0 document built from the compiled routes: paths with `{param}` placeholders, path/query/header parameters taken from what each route actually reads, request bodies for routes that use `body`, and the status codes each route can answer including guard failures.

```sh
velo openapi examples/todo.velo > openapi.json
```

A route can serve a page next to the API, read once at compile time:

```velo
GET /docs => file("docs.html")
```

`examples/shop/docs.html` is a small page that fetches `/openapi.json` and lists the routes. Because the contents are folded into the route, serving it is a `memcpy` (80k req/s in the same benchmark as `/health`), and a missing file is a compile error rather than a 404 at runtime.

The same document can be served by the API itself. `openapi()` is folded to constant bytes at compile time, so the route costs a `memcpy` like any other constant (84k req/s in the same benchmark as `/health`):

```velo
GET /openapi.json => openapi()
```

`VELO_TITLE` sets the document title.

## Watch mode

`--watch` supervises the server and restarts it whenever the route file or anything it includes changes:

```sh
velo run examples/shop/app.velo :8080 --watch
```

Restarts go through `SIGTERM`, so a `--data` snapshot is flushed first. A file that fails to compile leaves the supervisor running: it prints the error, keeps watching, and starts again on the next save.

## Persistence

The store is in memory. Pass `--data file.json` (or set `VELO_DATA`) and velo loads that file at boot and writes it back whenever the data changed, at most once every `VELO_SAVE_MS` milliseconds (default 200):

```sh
velo run examples/api.velo :8080 --data data.json
```

Saves are atomic (write to `.tmp`, then rename) and skipped entirely when nothing was written, so a read-only workload never touches the disk. The gap between snapshots adapts: velo measures how long the last save took and waits until that save has cost at most `VELO_SAVE_DUTY` percent of wall time, so a 5 MB dataset under sustained writes is not rewritten five times a second. `SIGINT` and `SIGTERM` stop the event loop and write a final snapshot before exiting, so an orderly shutdown loses nothing; a hard kill can lose at most the last save interval.

## Rate limiting

`VELO_RATE=100` allows each client 100 requests per second and answers `429 Too Many Requests` beyond that, counted in one-second windows across 16 shards. Behind a proxy every socket looks like `127.0.0.1`, so set `VELO_REAL_IP_HEADER=CF-Connecting-IP` (or `X-Forwarded-For`) to key on the real client — only trust that header if a proxy you control sets it.

The check costs a lock and a hash per request. In a loopback benchmark, where every request shares one key and all workers contend on the same shard, `/health` drops from 90.6k to 71.9k req/s; real traffic spread over many client IPs spreads over the shards.

## Logging

`VELO_LOG=1` writes one line per request to stderr:

```
GET /users?x=1 200 2b 7us
POST /users 201 19b 21us
```

`VELO_LOG=json` writes the same fields as one JSON object per line, ready for a log shipper:

```json
{"method":"POST","path":"/users","status":201,"bytes":19,"micros":21}
```

Logging writes to stderr on the worker thread and includes a clock read per request: `/health` drops from 82.3k to 66.1k req/s with it on. Leave it off in production, or point stderr at a file.

## Metrics

Set `VELO_METRICS=/_metrics` and that path answers:

```json
{"version":"0.26.0","uptime_ms":3747,"requests":275021,"failures":1,"connections":1,
 "bytes_out":5500420,"avg_micros":15,"max_micros":38,"routes":23,"workers":4}
```

`failures` counts responses velo generated itself (404, 405, 400, 401, 409, 413, and store misses), `connections` is the live count across workers. `avg_micros` and `max_micros` measure the time from parsed request to rendered response. Timing costs a clock read per request, so enabling metrics trades about 9% of peak throughput (94.0k to 85.3k req/s on `/health`); everything else is relaxed atomics. Point a monitor at it, or at any route in your API.

## Deployment

Velo is one static binary and one text file. `deploy/` holds ready-to-copy templates:

| file | what it is |
| --- | --- |
| `deploy/velo.service` | systemd unit with `SIGTERM` shutdown, metrics, rate limiting, and sandboxing |
| `deploy/Caddyfile.snippet` | reverse proxy that terminates TLS, compresses, and forwards the client IP |
| `deploy/cloudflared.snippet.yml` | tunnel ingress for the same hostname |

```sh
sudo install -m755 target/x86_64-unknown-linux-musl/release/velo /usr/local/bin/velo
install -Dm644 deploy/velo.service ~/.config/systemd/user/velo.service
systemctl --user daemon-reload && systemctl --user enable --now velo
```

`SIGTERM` is the clean stop: the event loop unwinds and the final snapshot is written before exit. Behind a proxy on the same host you can skip TCP entirely:

```sh
velo run /srv/api/app.velo unix:/run/velo/api.sock --data /srv/api/data.json
```

```
reverse_proxy unix//run/velo/api.sock
```

A stale socket file is replaced at startup unless something is still listening on it, and it is removed on shutdown. Velo speaks plain HTTP/1.1, so put a TLS terminator in front of it; that proxy is also what compresses responses. Behind a proxy every socket looks local, so set `VELO_REAL_IP_HEADER` to whatever header your proxy sets (`CF-Connecting-IP` behind Cloudflare, `X-Forwarded-For` otherwise) if you rate-limit, and only trust that header when the proxy is the only way in.

## Design

- **Const folding.** Routes whose expression touches no param, body, or store are evaluated at compile time and stored as ready-to-send bytes. `GET /health => "ok"` costs one `memcpy` per request.
- **Router.** Per-method exact map (FNV-hashed) for static paths, a segment tree for `:param` paths. Params are borrowed slices of the request line, never copied.
- **Values.** `Value` is an enum with `Arc` payloads, so returning a whole collection is a refcount bump, not a deep copy. JSON is written straight into the connection's output buffer.
- **Object routes render straight into the socket buffer.** A route whose body is an object or array literal is written directly as JSON bytes; no intermediate `Value` tree is built per request.
- **Rendered-once JSON.** Every stored row keeps its JSON bytes next to its fields, and each collection caches the JSON of its full row list and up to 32 sort orders and filters, inside a byte budget; all of it is rebuilt only when the collection is written to. Each worker also keeps a thread-local map of the results it has already seen, tagged with a collection version, so a cache hit costs an atomic load and a local lookup instead of a lock shared by every worker. The thread-local view holds pointers to the same bytes, and is bounded by both an entry count and `VELO_LOCAL_CACHE_BYTES` so superseded results cannot pile up. `GET /users` and `order(...)` are then a `memcpy`, not a sort and a serialization pass. Inserting a row appends to the cached list JSON in place when nothing else is holding it, and only when the list was actually read since the last write. A write-only burst therefore keeps its O(1) insert, and an alternating write/read workload neither re-renders nor recopies the list. The cost is holding rows twice in memory.
- **No allocation for key lookups.** `db.users.find(id)` on a plain path param hashes the slice of the request line directly; nothing is copied unless the param is percent-encoded.
- **Store.** Copy-on-write snapshot behind an `RwLock`; readers clone an `Arc<Vec<Value>>` and release the lock immediately.
- **HTTP.** Hand-written HTTP/1.1: keep-alive by default, request pipelining, per-connection read/write/body buffers reused across requests, batched writes. A connection stops rendering further pipelined requests once 256 kB of response bytes are waiting, so a client cannot make the server buffer an unbounded amount by pipelining requests for large lists; it resumes as soon as the socket drains. `Date` is formatted once per second per worker, not per response. `Expect: 100-continue` gets its interim response as soon as the headers arrive. The scan for the end of the headers resumes where it stopped, so a client feeding headers one byte at a time costs linear work, not quadratic. Chunked bodies are refused with 411, conflicting `Content-Length` headers with 400, oversized bodies with 413, oversized headers with 431.
- **Event loop.** One epoll instance per worker thread (default: one per core), all sharing the listener with `EPOLLEXCLUSIVE`. Connections are non-blocking and cost a few kB each instead of a thread and a stack: 1 000 live connections fit in under 1 MB of RSS. `epoll` is called through three `extern "C"` declarations, still no crates.
- **No dependencies.** `[dependencies]` is empty. std only.

Env knobs:

| variable | default | effect |
| --- | --- | --- |
| `VELO_ADDR` | `:8080` | listen address, TCP or `unix:<path>` |
| `VELO_SOCKET_MODE` | `660` | permissions for a Unix socket, octal |
| `VELO_WORKERS` | cores | event loops, one thread each |
| `VELO_MAX_CONNS` | 65536 | live connections per worker, extra ones get 503 |
| `VELO_KEEPALIVE` | 60 | idle seconds before a served connection is swept |
| `VELO_HEADER_TIMEOUT` | 10 | seconds a connection may spend before its first complete request; drip-feeding headers does not extend it |
| `VELO_DATA` | off | snapshot file, same as `--data` |
| `VELO_SAVE_MS` | 200 | minimum gap between snapshots |
| `VELO_SAVE_DUTY` | 10 | percent of wall time a snapshot may cost; the gap grows with the file so big datasets are not rewritten every 200 ms |
| `VELO_HEADERS` | off | extra response headers, e.g. `X-Content-Type-Options: nosniff; Cache-Control: no-store` |
| `VELO_CORS` | off | value for `Access-Control-Allow-Origin`; also answers `OPTIONS` preflight with 204 |
| `VELO_LOG` | off | one line per request on stderr: `1` for text, `json` for one JSON object per line |
| `VELO_METRICS` | off | path that answers a metrics JSON, e.g. `/_metrics` |
| `VELO_TITLE` | `velo api` | title used by `openapi()` |
| `VELO_CACHE_BYTES` | 8 MB | budget for the shared rendered-result cache; exceeding it clears it |
| `VELO_LOCAL_CACHE_BYTES` | 1 MB | per-worker budget for its thread-local view of those results |
| `VELO_RATE` | off | requests per second allowed per client; over it answers 429 |
| `VELO_REAL_IP_HEADER` | off | header holding the client IP behind a proxy, e.g. `CF-Connecting-IP`; without it the socket address is used |
| `VELO_ETAG` | off | send `ETag` on 200 `GET`/`HEAD` responses and answer 304 to a matching `If-None-Match` |

## Benchmarks

Load generator: `velobench` (ships in this repo, thread per connection, keep-alive). 4-core box, client and server share the machine, release build, v0.41.1. The `users` collection holds 501 rows (16 kB as JSON). The `users` collection holds 200 rows.

`-c 50`, one request in flight per connection — client-bound, both processes fight for the same 4 cores:

| route | kind | req/s |
| --- | --- | --- |
| `/health` | const fold | 91 900 |
| `/users/:id` | store lookup | 91 200 |
| `/stats` | 2 store counts | 86 700 |
| `/users` (501 rows, 21 kB) | cached list | 65 400 |
| `/users/by/team` | cached filter | 59 800 |
| `POST /users` | JSON parse + insert | 53 900 |

`-c 8 -p 32`, pipelined — what the server itself can do:

| route | req/s |
| --- | --- |
| `/health` | 835 000 |
| `/stats` | 785 000 |
| `/scores` (3 aggregates) | 368 000 |
| `/users` (21 kB each) | 243 000 |
| `/users/sorted` | 158 000 |
| `/users/by/team` | 131 000 |

In-process, no sockets, one thread (`velomicro <rows>`). `bench/baseline.json` records these on this machine, and `velomicro --check` fails the build if any of them regresses past `VELO_PERF_LIMIT` (3x by default) — that guard exists because a cache-key mistake once made `order` 200x slower while every test stayed green:

| operation | 500 rows | 20 000 rows |
| --- | --- | --- |
| `find(id)` | 0.23 us | 0.26 us |
| `where` (cached) | 1.1 us | 1.2 us |
| `create` | 2.3 us | 3.5 us |
| `order` (cached) | 5.0 us | 50 us |
| `all` (cached) | 5.7 us | 51 us |
| `create` + `delete` | 3.1 us | 3.4 us |
| `create` then read the whole list | 8.9 us | 62 us |

`find`, `create`, `delete`, and cached filters stay flat. Anything that hands back the whole collection is bound by the bytes it copies.

Deleting a row leaves a tombstone in place so the surviving rows keep their positions and insertion order; the collection is compacted once tombstones pass half the rows.

Run read benchmarks before write benchmarks, or restart in between: a `POST` run at 50k req/s adds a hundred thousand rows and every later list measurement is then measuring a much bigger response.

For scale, the same client and box against a Go 1.26 `net/http` server serving equivalent responses (a constant `ok`, one JSON row, and a precomputed 11 kB list):

| route | Go `net/http` | velo |
| --- | --- | --- |
| `/health`, `-c 50` | 48 700 req/s | 89 100 req/s |
| `/users/:id`, `-c 50` | 49 200 req/s | 89 000 req/s |
| list, `-c 50` | 61 500 req/s | 72 600 req/s |
| `/health`, `-c 8 -p 32` | 57 100 req/s | 812 000 req/s |
| RSS while serving | 11.9 MB | 1.2 MB |
| binary | 8.6 MB | 0.6 MB |

Read that as a sanity check, not a verdict: `net/http` is a general server with middleware, HTTP/2, and dynamic handlers, while velo compiles a fixed route set and precomputes most of what it sends. The pipelining gap is mostly that `net/http` answers pipelined requests one at a time.

Over a Unix socket instead of loopback TCP, same server and client:

| route | TCP | Unix socket |
| --- | --- | --- |
| `/health`, `-c 50` | 86 900 req/s | 124 400 req/s |
| `/users/:id`, `-c 50` | 89 000 req/s | 113 800 req/s |
| `/health`, `-c 8 -p 32` | 812 000 req/s | 845 000 req/s |

Skipping the loopback TCP stack is worth about 30% when the proxy sits on the same host.

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

82 tests (61 integration + 12 CLI + 6 fuzz + 3 unit): const folding, CRUD, params, body fields, error codes, JSON round-trip and escaping, query params, percent-decoding, `where` filters, persistence round-trip, status overrides, paging, list-cache invalidation, graceful shutdown, built-ins, CORS preflight, sorting, compile-error formatting, `Date` formatting, header hardening, sort-cache and filter-cache invalidation, request headers, guards, client-supplied ids, metrics, ETag round-trip, rate limiting, raw-socket HTTP (keep-alive, pipelining, HEAD, chunked rejection, split requests, 100 concurrent connections), concurrent writes, and a read/write stress test that hammers the list, sort, filter, search, and aggregate caches from five reader threads while four writers insert, then checks the final data is consistent.

`tests/cli.rs` drives the built binary end to end: `check` exit codes and error text, `new` refusing to overwrite, `openapi` output parsed back as JSON, a metrics endpoint, `include` across a directory of files, serving on a Unix socket, `--watch` restarting on a change and surviving a broken save, and a `POST` surviving a `SIGTERM` restart through the snapshot file.

`tests/fuzz.rs` adds six deterministic robustness tests: 2 000 mutated sources and 2 000 random byte strings through the compiler, 300 connections of malformed and truncated HTTP, 400 connections carrying byte-level mutations of otherwise valid requests (every answer must still be a well-formed status line), and oversized header and body requests. They also cover slow drip-feeding clients. `VELO_FUZZ_ROUNDS` raises the iteration counts for a longer hunt; 40 000 compiler mutations and 4 000 mutated requests have been run clean. They assert the process never panics and that the server still answers a normal request afterwards.

## CI

`./check.sh` is the gate: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, the full test suite, a release build, all three examples compiled, a performance guard against `bench/baseline.json`, and a boot smoke test with a short benchmark. `.github/workflows/ci.yml` runs exactly the same steps on push.

## Errors

Compile errors carry a line, a column, the source line, and a caret:

```
velo: app.velo: line 2:15: unknown identifier "user"
  2 | GET /users => user.all()
    |               ^
```

`velo check` exits non-zero on the first error, which is what `--watch` and CI use.

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
| `src/socket.rs` | TCP and Unix listeners behind one type |
| `src/value.rs` | `Value`, JSON reader and writer |
| `src/date.rs` | `Date` header formatting |
| `src/openapi.rs` | OpenAPI 3.0 document generation |
| `src/main.rs` | CLI |
| `src/bin/velobench.rs` | load generator |
| `src/bin/velomicro.rs` | in-process dispatch microbenchmark, `velomicro [rows]` |

## Build notes

`.cargo/config.toml` targets `x86_64-unknown-linux-musl` with `rust-lld`, so the build needs no system C toolchain. Remove that file to build against glibc with `cc`.

Requirements: Linux 4.5 or newer (the workers share the listener with `EPOLLEXCLUSIVE`), Rust 1.75 or newer, no crates.

## Changelog

**v0.41.1** — `velobench` can drive a Unix socket, which measured the gain: 86.9k to 124.4k req/s on `/health` compared with loopback TCP.

**v0.41.0** — `velo run app.velo unix:/run/velo.sock` listens on a Unix socket: stale files are replaced, permissions come from `VELO_SOCKET_MODE`, and the socket is removed on shutdown.

**v0.40.0** — compile errors now report a column and underline the offending token with a caret; the lexer tracks column positions for every token.

**v0.39.0** — `velomicro --json` and `velomicro --check <baseline>`: the microbenchmark can now fail the build when an operation regresses past a multiple of the recorded baseline. `check.sh` runs it.

**v0.38.0** — conditions combine with short-circuiting `and` / `or`. The keyword that sets a failed guard's status moved from `or` to `else` to free `or` for boolean use: `when body.name else 400`.

**v0.37.0** — expressions gained `+ - * /`, `< > <= >=`, and parentheses, with the usual precedence. Numbers written as strings (path params, query values) take part in arithmetic; `+` on two strings concatenates; mixed-type comparisons are false rather than surprising. Pure arithmetic still folds to a constant at compile time.

**v0.36.1** — `velo routes` prints where each route came from, which matters once files include each other; dropped an unused store method and fixed the quoting in the systemd template.

**v0.36.0** — `deploy/` templates: a hardened systemd unit, a Caddy reverse-proxy block that forwards the client IP, and a cloudflared ingress entry.

**v0.35.1** — benchmarked against a Go `net/http` server on the same box for scale: 1.8x the throughput on small responses, 14x pipelined, a tenth of the memory.

**v0.35.0** — `VELO_HEADERS` adds fixed response headers (security or caching policy), rejecting malformed entries and anything carrying control characters.

**v0.34.0** — request logs now carry the response size and duration, and `VELO_LOG=json` emits one JSON object per line.

**v0.33.0** — `file("page.html")` folds a file into a route at compile time and serves it with a content type derived from its extension; response content types are now plain strings throughout. `examples/shop` gained a `/docs` page listing the API from its own OpenAPI document.

**v0.32.1** — fuzz iteration counts are configurable with `VELO_FUZZ_ROUNDS`; an extended run (40 000 compiler mutations, 4 000 mutated requests) found only a divide-by-zero in the fuzz harness itself, now fixed.

**v0.32.0** — a request body that is not JSON is retried as `application/x-www-form-urlencoded`, so HTML forms and `curl --data-urlencode` work without changing a route.

**v0.31.0** — rendered bytes are held as `Arc<Vec<u8>>`, so building a cache entry no longer copies it and an insert can extend the cached list in place. A create-then-list cycle dropped from 1 381 us to 62 us on 20 000 rows and from 222 us to 8.9 us on 500; `/users` pipelined went 243k to 308k req/s.

**v0.30.1** — the header terminator scan resumes from where it left off instead of restarting on every read, so drip-fed headers cost linear work.

**v0.30.0** — `velo run --watch` restarts the server when the route file or any included file changes, stopping the old process with `SIGTERM` so snapshots flush, and surviving files that fail to compile.

**v0.29.1** — `openapi()` and `velo openapi` now describe included files too; the document is built after the merge. Added `examples/shop/`, a four-file API using `include`.

**v0.29.0** — `include "other.velo"` merges route files, resolved relative to the including file, with repeats skipped so cycles are harmless. Every CLI command loads through the same path.

**v0.28.0** — deleting is now constant time: rows are tombstoned instead of shifted, and the collection compacts when tombstones exceed half the rows. A create/delete cycle on 20 000 rows went from 52 us to 3.4 us.

**v0.27.1** — measured the store at 20 000 rows and published the numbers: `find` and `create` stay flat, list-shaped operations are bound by the bytes they copy, `delete` is linear because insertion order is preserved.

**v0.27.0** — inserting a row now appends to the cached list JSON when the list has been read since the last write, cutting a write-then-list cycle from 309 us to 191 us on 4 500 rows; write-only bursts keep their previous cost (56k inserts/s).

**v0.26.2** — README reordered into a reading path with a table of contents.

**v0.26.1** — fuzz suite now mutates valid requests byte by byte and asserts every answer is still a well-formed HTTP status line. No defects found; kept as a regression net.

**v0.26.0** — metrics now report `bytes_out`, `avg_micros`, and `max_micros`, measured only when `VELO_METRICS` is set.

**v0.25.1** — the per-worker cache view is bounded by bytes as well as entries, so invalidated large results are released instead of being held by every worker until 64 entries accumulate.

**v0.25.0** — two evaluation changes: object and array routes render straight into the output buffer instead of building a `Value` tree (`/stats` 417k to 785k req/s pipelined), and each worker keeps a thread-local, version-tagged view of the derived caches so hits no longer serialize on one lock (`order` 55k to 158k, `where` 50k to 131k req/s).

**v0.24.0** — aggregations: `sum`, `avg`, `min`, `max` over a numeric field, cached and invalidated with the other derived results. Non-numeric and missing values are skipped.

**v0.23.1** — stress test covering cache invalidation under concurrent reads and writes.

**v0.23.0** — `db.x.search(field, text)`: case-insensitive substring match over a field, cached and invalidated like `where` and `order`.

**v0.22.0** — building on a non-Linux target now fails with a plain message instead of a link error, and the platform requirements are stated up front.

**v0.21.2** — bounded response buffering: pipelined requests stop being rendered past 256 kB of pending output and resume after the flush. 100 pipelined 16 kB responses now cost 1.1 MB of RSS instead of growing with the batch, at the same throughput.

**v0.21.1** — end-to-end CLI tests: the real binary is started, driven over TCP, stopped with `SIGTERM`/`SIGINT`, and restarted to prove the snapshot round-trips.

**v0.21.0** — `openapi()` built-in: a route can serve this API's own OpenAPI document, folded to constant bytes at compile time.

**v0.20.0** — per-client rate limiting: `VELO_RATE` requests per second, keyed on the socket address or on `VELO_REAL_IP_HEADER` behind a proxy, answered with 429.

**v0.19.1** — snapshot interval is now self-tuning: a save may cost at most `VELO_SAVE_DUTY` percent of wall time (10 by default). Under a sustained 45k writes/s load on a 5 MB dataset that cut disk writes from 53 MB to 42 MB per 5 s while running slightly faster.

**v0.19.0** — `velo openapi` generates an OpenAPI 3.0 document from the compiled routes. The compiler now records which query and header fields each route reads, so the document lists real parameters instead of guesses.

**v0.18.2** — the render caches now respect a byte budget (`VELO_CACHE_BYTES`, 8 MB by default) instead of only an entry count, so 32 large lists cannot quietly hold hundreds of megabytes.

**v0.18.1** — `where` results are cached per field and value alongside the sorted and full-list caches, all cleared on any write (`where` 77 us to 1.2 us per call on 5 000 rows). `velobench` now parses `Content-Length` instead of scanning every byte, so large responses measure the server rather than the client.

**v0.18.0** — `when <condition> or <status>` picks the status a failed guard answers, so a guard doubles as body validation (`when body.name else 400`). `velo routes` now prints each route guard.

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
