# Velo

**v1.9.0** — a tiny language for HTTP APIs, written in Rust with zero dependencies. One line per endpoint, compiled to an expression tree, served by an epoll event loop.

```velo
GET    /health     => "ok"
GET    /users      => db.users.all()
GET    /users/:id  => db.users.find(id)
POST   /users      => db.users.create(body) when body.name else 400
PUT    /users/:id  => db.users.upsert(id, body)
DELETE /users/:id  => db.users.delete(id) : 204
GET    /search     => db.users.where("team", query.team)
GET    /stats      => { users: db.users.count(), avg: db.users.avg("score") }
```

That file is a complete, running API server.

Linux only: the event loop is epoll, and the build stops with a clear message anywhere else.

[Scope](#scope) · [Quick start](#quick-start) · [Language](#language) · [Guards](#guards) · [OpenAPI](#openapi) · [Embedding](#embedding) · [Persistence](#persistence) · [Rate limiting](#rate-limiting) · [Metrics](#metrics) · [Deployment](#deployment) · [Design](#design) · [Benchmarks](#benchmarks) · [Tests](#tests) · [CI](#ci) · [Layout](#layout) · [Build notes](#build-notes) · [Changelog](#changelog)

## Scope

Velo suits a JSON API whose data fits in memory on one machine: an internal service, a mobile or web backend, a prototype that has to be fast, a sidecar that shapes data for something else. It compiles a fixed set of routes and precomputes as much of each response as it can.

It is deliberately not: a database (the store is memory-bound, single-process, snapshot-persisted), a TLS terminator, an HTTP/2 or WebSocket server, a file-upload endpoint, or a cluster. Put a proxy in front for TLS, compression, and HTTP/2, and keep the dataset within RAM.

Protocol notes: HTTP/1.1 with keep-alive and pipelining; HTTP/1.0 works and closes unless the client asks otherwise; request targets are origin-form (`/path?query`), so proxy-style absolute URLs answer 404; chunked request bodies are refused with 411; methods are case-sensitive, as the spec says.

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
| query string | `query.limit` | read straight from the request, percent-decoded; naming a field costs one scan, not a parse of the whole string |
| request header | `header.x_team` | lowercased, `-` written as `_`, read straight from the request; the first header of that name wins |
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
| `first(field, value)` | first matching row, `first(field, op, value)` to compare | 404 |
| `where(field, value)` | array of matching rows; linear scan, cached per field and value | `[]` |
| `where(field, op, value)` | same, with `op` one of `== != < <= > >=` | `[]` |
| chained steps | `where`, `search`, `order` and `page` compose; see below | |
| `page(offset, limit)` | slice of rows, `limit` 0 means "to the end" | `[]` |
| `search(field, text)` | rows whose `field` contains `text`, case-insensitive | `[]` |
| `order(field)` | rows sorted by `field`, `"-field"` for descending | `[]` |
| `create(value)` | the stored row; `id` is generated unless the value carries one | 400 on an empty body, 409 on a duplicate `id` |
| `update(key, patch)` | merged row | 404 |
| `upsert(key, value)` | merged row, or a new row keyed by `key` | never misses |
| `delete(key)` | `{"deleted":true}` | 404 |
| `delete_where(field, value)` | `{"deleted":n}` | `{"deleted":0}` |
| `clear()` | `{"deleted":n}`, resets generated ids | `{"deleted":0}` |
| `select(field, ...)` | the rows with only those fields kept | `[]` |

Read operations chain, so one line can filter, sort and page:

```velo
GET /users/top   => db.users.where("team", query.team).order("-score").page(0, 20)
GET /users/band  => db.users.where("score", ">=", query.lo).where("score", "<=", query.hi).order("score")
GET /users/hits  => db.users.search("name", query.q).count()
GET /users/spend => db.users.where("team", query.team).sum("score")
GET /users/best  => db.users.where("team", query.team).order("-score").first()
GET /users/cards => db.users.where("team", query.team).select("id", "name")
```

`select` ends a chain by narrowing each row to the fields it names, in the order it names them, skipping any the row does not have. It is how a route keeps a password hash or an internal note out of the response, and it is the cheapest way to shrink a large list: over 5 000 rows, trading five fields for two took the body from 509 kB to 149 kB and the route from 6 437 to 19 125 req/s, all of it saved on the wire.

```velo
GET /users        => db.users.select("id", "name", "team")
GET /users/public => db.users.where("active", true).order("name").select("id", "name")
```

A filter compares with `==` by default, and that is the case the collection indexes, so `where("team", query.team)` stays cheap as the collection grows even while it is being written to. Pass an operator to widen it:

```velo
GET /orders/big  => db.orders.where("amount", ">", 100)
GET /orders/open => db.orders.where("status", "!=", "done").count()
```

The operator is a literal, checked at compile time. A comparison is numeric when both sides read as numbers, otherwise it compares text; a row missing the field never matches. `where`, `search`, `order` and `page` are steps and may repeat in any order. `count()`, `sum/avg/min/max(field)`, `select(field, ...)` and `first()` end a chain and nothing may follow them; `first()` answers 404 when the chain is empty. A chained result is cached like a single call, keyed on the whole chain, and thrown away when the collection changes. `first()` is the one shape that is not cached: it scans instead, and a trailing `order` picks the extreme row in one pass rather than sorting.

A row's field can be read directly:

```velo
GET /users/:id/name => db.users.find(id).name
GET /leader         => db.users.order("-score").first().name
```

Built-in functions:

| function | returns |
| --- | --- |
| `now()` | Unix time in milliseconds |
| `date(ms)` | that instant as `YYYY-MM-DDTHH:MM:SSZ`, or `null` if it is not a number |
| `uuid()` | random v4 UUID string |
| `len(x)` | length of a string, array, or object (`null` is 0) |
| `env("NAME")` | environment variable, or `null` |
| `default(x, fallback)` | `x`, unless it is `null` or an empty string |
| `lower(x)` / `upper(x)` | text in one case, `null` stays `null` |
| `trim(x)` | text without surrounding whitespace |
| `hash(x)` | SHA-256 of the text, lowercase hex |
| `password(x)` | a salted PBKDF2-HMAC-SHA256 digest of the text, safe to store |
| `verify(x, stored)` | whether `x` is the password behind `stored` |
| `openapi()` | this API's OpenAPI 3.0 document, rendered once at compile time |
| `file("page.html")` | the file's contents, read at compile time, served with a content type from its extension |

Everything but `now()`, `uuid()`, `date()`, `password()`, `verify()` and `openapi()` is folded at compile time when its arguments are constant, so `upper("velo")` costs nothing at runtime.

```velo
GET  /users      => db.users.page(default(query.offset, 0), default(query.limit, 20))
POST /users      => db.users.create({ name: trim(body.name), email: lower(trim(body.email)) })
GET  /scores     => { total: db.orders.sum("amount"), avg: db.orders.avg("amount") }
POST /todos      => db.todos.create({ id: uuid(), at: date(now()), text: body.text })
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

A guard that fails answers the guard status, and so does a guard that hits an error. `when verify(body.pass, db.users.find(body.email).pass) else 401` answers `401` whether the password was wrong or the account does not exist, so a login route cannot be used to enumerate accounts.

## Auth

`password(text)` turns a password into something safe to keep, and `verify(text, stored)` checks one against it. The digest is PBKDF2-HMAC-SHA256 with a fresh 16-byte salt per call, stored as `pbkdf2$<rounds>$<salt>$<digest>`; `VELO_KDF_ROUNDS` sets the work factor and defaults to 100 000. Verification compares in constant time, and the same password hashed twice gives two different strings.

```velo
POST /signup => db.users.create({ id: lower(body.email), email: lower(body.email), pass: password(body.pass) }).email when body.email and body.pass else 400
POST /login  => db.sessions.create({ id: uuid(), user: lower(body.email), until: now() + 86400000 }) when verify(body.pass, db.users.find(lower(body.email)).pass) else 401
GET  /me     => db.sessions.find(header.x_token).user when db.sessions.where("id", header.x_token).count() and db.sessions.find(header.x_token).until > now()
DELETE /logout => db.sessions.delete(header.x_token) : 204 when db.sessions.where("id", header.x_token).count()
```

The work factor is the point: a login costs one full PBKDF2, which is 25 req/s at the default 100 000 rounds over four connections on the 4-core box below, and that slowness is what makes a stolen digest expensive to crack. Nothing else pays it. Checking the session token on every request afterwards is an ordinary indexed lookup, in the same band as any other guarded route.

A session is a row like any other, so a token is checked with a `where(...).count()` guard, which the equality index answers without a scan, and expiry is a comparison against `now()`. `uuid()` draws from a SHA-256 generator seeded from `/dev/urandom`, so a token cannot be guessed from earlier ones. Never end a route on the row that holds the digest: `select` the fields a client may see, as `examples/auth.velo` does.

`hash(x)` is plain SHA-256 in lowercase hex, for fingerprints, cache keys, or storing an API key you only ever compare. It is not a password hash; use `password()` for those. With a constant argument it folds at compile time.

`examples/auth.velo` is a complete signup, login, session and logout API. `examples/todo.velo` is a complete todo API using uuid keys, timestamps, sorting, and filters. `examples/shop/` splits a larger API over four files with `include`: a catalog with search, orders keyed by a customer header, and an admin section behind a token guard.

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

`--watch` supervises the server and restarts it whenever the route file, anything it includes, or any file it folds in with `file()` changes:

```sh
velo run examples/shop/app.velo :8080 --watch
```

Restarts go through `SIGTERM`, so a `--data` snapshot is flushed first. A file that fails to compile leaves the supervisor running: it prints the error, keeps watching, and starts again on the next save.

## Embedding

Velo is a library as well as a binary. `examples/embed.rs` compiles a route file against a store you own, seeds it, answers a request in-process, and can then serve:

```rust
let store = Store::new();
let program = compile_file(Path::new("examples/todo.velo"), Some(store.clone()))?;
store.collection("todos").create(Value::object(&[("text", Value::str("ship it"))]));

let server = Server::new(program)?;
let mut out = Vec::new();
server.dispatch("GET", "/todos", b"", &mut out);          // no socket involved
server.serve(Listener::bind("127.0.0.1:8080")?)?;         // or take connections
```

`cargo run --example embed` runs it. Dispatching without a socket is also how the tests and `velomicro` measure the engine.

The library exposes `compile`, `compile_in`, `compile_file`, `Program`, `Route`, `Method`, `Server`, `Store`, `Value`, and the `ast`, `http`, `openapi`, `parser`, `router`, `socket`, `store`, and `value` modules. The event loop, lexer, and date formatter are internal.

## Persistence

The store is in memory, so a collection is bounded by RAM and a snapshot is the whole dataset. Budget roughly 600 bytes of RAM per row for a small row: the parsed fields, the rendered JSON kept beside them, and the id index. 270 000 rows of four fields load from a 14 MB snapshot in about 0.9 s and occupy 168 MB. One process owns a data file; pointing two servers at the same file will lose writes. Pass `--data file.json` (or set `VELO_DATA`) and velo loads that file at boot and writes it back whenever the data changed, at most once every `VELO_SAVE_MS` milliseconds (default 200):

```sh
velo run examples/api.velo :8080 --data data.json
```

Saves are atomic (write to `.tmp`, then rename) and skipped entirely when nothing was written, so a read-only workload never touches the disk. A save takes only a read lock and skips tombstoned rows as it writes, so it neither compacts nor blocks writers: inserting at 41 000 req/s into a 200 000-row collection continues while snapshots of 16 MB are being written. The gap between snapshots adapts: velo measures how long the last save took and waits until that save has cost at most `VELO_SAVE_DUTY` percent of wall time, so a 5 MB dataset under sustained writes is not rewritten five times a second. `SIGINT` and `SIGTERM` stop the event loop and write a final snapshot before exiting, so an orderly shutdown loses nothing. Shutdown drains: velo stops accepting, answers whatever is already in flight with `Connection: close`, and exits once the last connection is gone or `VELO_DRAIN_MS` passes. A client benchmarking through a `SIGTERM` sees zero errors. a hard kill can lose at most the last save interval.

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
{"version":"1.7.0","uptime_ms":3747,"requests":275021,"failures":1,"connections":1,
 "bytes_out":5500420,"avg_micros":15,"max_micros":38,"routes":23,"workers":4,
 "paths":[{"route":"GET /users","hits":181402,"failures":0,"avg_micros":19,"max_micros":38},
          {"route":"POST /users","hits":93619,"failures":1,"avg_micros":7,"max_micros":31}]}
```

`paths` breaks the same counters down by route, labelled `METHOD /pattern` as written in the source, so a slow or failing endpoint is visible without a tracing stack. A route appears once it has been served, so the list stays as small as the traffic; a request that matched no route counts in the totals but has no route to belong to. `failures` here is any answer of 400 or above from that route, guard rejections included.

`failures` counts responses velo generated itself (404, 405, 400, 401, 409, 413, and store misses), `connections` is the live count across workers. `avg_micros` and `max_micros` measure the time from parsed request to rendered response. Timing costs a clock read per request, so enabling metrics trades about 9% of peak throughput (94.0k to 85.3k req/s on `/health`); everything else, the per-route counters included, is relaxed atomics and does not move the number out of run-to-run noise. Point a monitor at it, or at any route in your API.

## Deployment

Velo is one static binary and one text file. `deploy/` holds ready-to-copy templates:

| file | what it is |
| --- | --- |
| `deploy/velo.service` | systemd unit with `SIGTERM` shutdown, metrics, rate limiting, and sandboxing |
| `deploy/velo.socket` | systemd socket unit; systemd owns the listener so restarts never refuse a connection |
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

A stale socket file is replaced at startup unless something is still listening on it, and it is removed on shutdown.

With `deploy/velo.socket` installed, systemd holds the listening socket and hands it over on start (`LISTEN_FDS`), so a restart queues connections in the kernel instead of refusing them:

```sh
install -Dm644 deploy/velo.socket ~/.config/systemd/user/velo.socket
systemctl --user enable --now velo.socket
systemctl --user restart velo    # in-flight requests drain, new ones wait
```

Verified on this machine: a client running 20 keep-alive connections through two consecutive `systemctl --user restart` calls completed 235 414 requests with zero errors. Velo speaks plain HTTP/1.1, so put a TLS terminator in front of it; that proxy is also what compresses responses. Behind a proxy every socket looks local, so set `VELO_REAL_IP_HEADER` to whatever header your proxy sets (`CF-Connecting-IP` behind Cloudflare, `X-Forwarded-For` otherwise) if you rate-limit, and only trust that header when the proxy is the only way in.

## Design

- **Const folding.** Routes whose expression touches no param, body, or store are evaluated at compile time and stored as ready-to-send bytes. `GET /health => "ok"` costs one `memcpy` per request.
- **Router.** Per-method exact map (FNV-hashed) for static paths, a segment tree for `:param` paths. Params are borrowed slices of the request line, never copied.
- **Values.** `Value` is an enum with `Arc` payloads, so returning a whole collection is a refcount bump, not a deep copy. JSON is written straight into the connection's output buffer.
- **Object routes render straight into the socket buffer.** A route whose body is an object or array literal is written directly as JSON bytes; no intermediate `Value` tree is built per request.
- **Rendered-once JSON.** Every stored row keeps its JSON bytes next to its fields, and each collection caches the JSON of its full row list and up to 32 sort orders and filters, inside a byte budget; all of it is rebuilt only when the collection is written to. Each worker also keeps a thread-local map of the results it has already seen, tagged with a collection version, so a cache hit costs an atomic load and a local lookup instead of a lock shared by every worker. The thread-local view holds pointers to the same bytes, and is bounded by both an entry count and `VELO_LOCAL_CACHE_BYTES` so superseded results cannot pile up. `GET /users` and `order(...)` are then a `memcpy`, not a sort and a serialization pass. Inserting a row appends to the cached list JSON in place when nothing else is holding it and the list was read since the last write, at any size. When a reader is holding those bytes the insert has to copy them instead, and `VELO_APPEND_MAX` caps that: past it the insert drops the cache, because copying a multi-megabyte list on every write is far worse than re-rendering it on the next read. Chained reads are cached the same way, keyed on the whole chain. Rendering happens outside the collection lock: a reader copies the live row handles under a brief lock, releases it, then renders, and the result is only cached if the collection did not change meanwhile. Writers never wait behind a long render, and never pay a copy-on-write of the row list because no reader holds it.

Filters, sorts, searches, and aggregates work the same way. On a large collection under a constant write load their caches are invalidated as fast as they are built. A write-only burst therefore keeps its O(1) insert, and an alternating write/read workload neither re-renders nor recopies the list. The cost is holding rows twice in memory.
- **Equality index.** When a chain starts with `where(field, value)`, the collection builds a map from that field's values to row positions on first use and keeps it beside the rows. The filter then visits only the matching rows instead of every row, which is what makes a cache miss cheap: the caches above are invalidated as fast as they are built under a hot writer, so the miss path is the path that matters. An insert appends to the index rather than dropping it, since it only ever adds one row at the end; every other kind of write drops it, so the index cannot outlive the rows it describes. Indexes are built only past 512 rows, for at most 8 fields per collection, and are not kept for a field with more than 65 536 distinct values. On 20 000 rows an index over a four-value field cost 132 kB, about 7 bytes a row.
- **Nothing is parsed unless it is read.** A route naming `query.limit` or `header.x_key` compiles to a lookup that scans the raw request bytes for that one name and decodes only its value; a request with eight headers costs no allocation for the seven the route ignores. Routes that use `query` or `header` as a whole object still get one built.
- **No allocation for key lookups.** `db.users.find(id)` on a plain path param hashes the slice of the request line directly; nothing is copied unless the param is percent-encoded.
- **Store.** Rows live in `Arc`-ed chunks of 128 behind an `RwLock`. A reader clones the chunk handles, which is one refcount bump per 128 rows rather than per row, and releases the lock before it renders. A write that lands while a reader holds those handles copies one chunk, never the whole collection, so neither side can be made slow by the other. Before chunking, a filter that missed the cache spent most of its time cloning 20 000 row handles: 2 346 us, against 377 us now.
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
| `VELO_DRAIN_MS` | 2000 | how long shutdown waits for in-flight connections to finish |
| `VELO_HEADER_TIMEOUT` | 10 | seconds a connection may spend before its first complete request; drip-feeding headers does not extend it |
| `VELO_DATA` | off | snapshot file, same as `--data` |
| `VELO_SAVE_MS` | 200 | minimum gap between snapshots |
| `VELO_SAVE_DUTY` | 10 | percent of wall time a snapshot may cost; the gap grows with the file so big datasets are not rewritten every 200 ms |
| `VELO_HEADERS` | off | extra response headers, e.g. `X-Content-Type-Options: nosniff; Cache-Control: no-store` |
| `VELO_CORS` | off | value for `Access-Control-Allow-Origin`; also answers `OPTIONS` preflight with 204 |
| `VELO_LOG` | off | one line per request on stderr: `1` for text, `json` for one JSON object per line |
| `VELO_METRICS` | off | path that answers a metrics JSON, e.g. `/_metrics` |
| `VELO_TITLE` | `velo api` | title used by `openapi()` |
| `VELO_APPEND_MAX` | 512 kB | above this, an insert being read concurrently drops the cached list instead of copying it |
| `VELO_CACHE_BYTES` | 8 MB | budget for the shared rendered-result cache; exceeding it clears it |
| `VELO_LOCAL_CACHE_BYTES` | 1 MB | per-worker budget for its thread-local view of those results |
| `VELO_RATE` | off | requests per second allowed per client; over it answers 429 |
| `VELO_REAL_IP_HEADER` | off | header holding the client IP behind a proxy, e.g. `CF-Connecting-IP`; without it the socket address is used |
| `VELO_ETAG` | off | send `ETag` on 200 `GET`/`HEAD` responses and answer 304 to a matching `If-None-Match`; constant routes carry a tag computed at compile time |

## Benchmarks

Load generator: `velobench` (ships in this repo, thread per connection, keep-alive). 4-core box, client and server share the machine, release build, v1.8.0. The `users` collection holds 500 rows (21 kB as JSON).

`-c 50`, one request in flight per connection — client-bound, both processes fight for the same 4 cores:

| route | kind | req/s | p50 |
| --- | --- | --- | --- |
| `/health` | const fold | 94 900 | 0.38 ms |
| `/users/:id` | store lookup | 90 600 | 0.44 ms |
| `/stats` | 2 store counts | 84 300 | 0.46 ms |
| `/users` (500 rows, 21 kB) | cached list | 68 500 | 0.63 ms |
| `/users/sorted` | cached sort | 60 800 | 0.66 ms |
| `/users/by/team` | cached filter | 54 200 | 0.74 ms |
| `POST /users` | JSON parse + insert | 51 700 | 0.73 ms |
| `/users/top` | chain: filter, sort, page 20 | 58 800 | 0.63 ms |
| `/users/top/count` | chain: filter, count | 60 700 | 0.61 ms |
| `/users/top/one` | chain: filter, best row | 33 900 | 1.15 ms |
| `/users/strong` | chain: `>=` filter, sort, page | 58 100 | 0.60 ms |

`-c 8 -p 32`, pipelined — what the server itself can do:

| route | req/s | transfer |
| --- | --- | --- |
| `/users/:id` | 705 000 | 121 MB/s |
| `/stats` | 695 000 | 108 MB/s |
| `/health` | 695 000 | 99 MB/s |
| `/users` (21 kB each) | 215 000 | 4.6 GB/s |
| `/scores` (3 aggregates) | 171 000 | 28 MB/s |
| `/gated` (header guard) | 644 000 | 92 MB/s |
| `{ a: query.a, b: query.b }` | 554 000 | 84 MB/s |
| `/users/sorted` | 137 000 | 3.0 GB/s |
| `/users/by/team` | 122 000 | 2.6 GB/s |

In-process, no sockets, one thread (`velomicro <rows>`). `bench/baseline.json` records these on this machine, and `velomicro --check` fails the build if any of them regresses past `VELO_PERF_LIMIT` (3x by default) — that guard exists because a cache-key mistake once made `order` 200x slower while every test stayed green:

| operation | 500 rows | 20 000 rows |
| --- | --- | --- |
| `find(id)` | 0.23 us | 0.24 us |
| `where` (cached, driven by `query.t`) | 0.65 us | 0.82 us |
| `create` | 2.2 us | 2.3 us |
| `create` + `delete` | 2.9 us | 3.0 us |
| `all` (cached) | 6.3 us | 50 us |
| `order` (cached) | 6.6 us | 53 us |
| `create` then read the whole list | 7.8 us | 57 us |
| `where` + `order` + `page` chain (cached) | 1.6 us | 2.1 us |

A chain costs about what a single cached filter costs, and stays flat as the collection grows, because only the result of the whole chain is rendered and cached. `first()` is the exception: it scans on every request, which is why `/users/top/one` sits at half the throughput of the others.

`find`, `create`, `delete`, and cached filters stay flat. Anything that hands back the whole collection is bound by the bytes it copies.

Deleting a row leaves a tombstone in place so the surviving rows keep their positions and insertion order; the collection is compacted once tombstones pass half the rows.

Mixed read/write load on a 186 000-row collection, one reader looping over a whole-collection query while 20 connections insert:

| reader | writes before | writes after |
| --- | --- | --- |
| `/users` (cached list) | 154 req/s | 15 500 req/s |
| `/users/by/team` (filter) | 1 091 req/s | 33 500 req/s |
| `/users/sorted` (sort) | 779 req/s | 32 900 req/s |

At a larger scale, 554 000 rows and a 40 MB list, the same shape now runs at 26 800 writes/s with a reader looping `GET /users` against 46 800 with no reader at all. Before v1.1.0 a list that size was past `VELO_APPEND_MAX`, so every insert threw the cached list away and every read rebuilt all 40 MB.

Write tail latency in that test fell with it: the worst insert went from 1 150 ms to 49 ms on the sort case. Writes with no reader at all run at about 41 000 req/s, so a heavy reader now costs roughly 20% of write throughput instead of 98%.

A harder mix, five reader streams on different chain shapes plus a writer stream, all flat out against a collection growing past 32 000 rows, before and after chunked rows (v1.5.0):

| stream | before | after |
| --- | --- | --- |
| `where` + `order` + `page(0,20)` | 367 req/s | 1 616 req/s |
| `where` + `count` | 375 req/s | 1 352 req/s |
| `page(offset, 50)` | 227 req/s | 526 req/s |
| `where("score", ">=", n)` + `count` | 110 req/s | 303 req/s |
| `where` + `order` + `first()` | 124 req/s | 363 req/s |
| `POST /users` | 184 req/s | 534 req/s |

Nothing here is cached: the writer invalidates every collection cache faster than the readers can fill it, so each of those reads is a full scan. Writes speed up too, because the readers stop hogging the machine. Zero errors on both runs, and the counts add up afterwards.

A soak run (mixed reads and writes, millions of requests) holds steady: read-only load keeps RSS at 1.17 MB across 3.6 M requests, and memory otherwise tracks the data, not the traffic.

Run read benchmarks before write benchmarks, or restart in between: a `POST` run at 50k req/s adds a hundred thousand rows and every later list measurement is then measuring a much bigger response.

For scale, the same client and box against a Go 1.26 `net/http` server serving equivalent responses (a constant `ok`, one JSON row, and a precomputed 11 kB list):

| route | Go `net/http` | velo |
| --- | --- | --- |
| `/health`, `-c 50` | 48 700 req/s | 89 100 req/s |
| `/users/:id`, `-c 50` | 49 200 req/s | 89 000 req/s |
| list, `-c 50` | 61 500 req/s | 72 600 req/s |
| `/health`, `-c 8 -p 32` | 57 100 req/s | 812 000 req/s |
| RSS while serving | 11.9 MB | 1.2 MB |
| binary | 8.6 MB | 0.86 MB |

Read that as a sanity check, not a verdict: `net/http` is a general server with middleware, HTTP/2, and dynamic handlers, while velo compiles a fixed route set and precomputes most of what it sends. The pipelining gap is mostly that `net/http` answers pipelined requests one at a time.

Over a Unix socket instead of loopback TCP, same server and client:

| route | TCP | Unix socket |
| --- | --- | --- |
| `/health`, `-c 50` | 94 900 req/s | 111 500 req/s |
| `/users/:id`, `-c 50` | 90 600 req/s | 111 500 req/s |
| `/health`, `-c 8 -p 32` | 695 000 req/s | 871 000 req/s |

Skipping the loopback TCP stack is worth 15-25% when the proxy sits on the same host.

Connection scaling (`/health`), server RSS while serving:

| conns | req/s | p50 | RSS |
| --- | --- | --- | --- |
| 50 | 80 400 | 0.47 ms | 1.45 MB |
| 500 | 66 700 | 5.70 ms | 1.61 MB |
| 1 000 | 62 600 | 12.97 ms | 1.74 MB |

Binary: 856 kB, statically linked.

Reproduce:

```sh
./bench.sh
velobench -c 8 -p 32 -d 5 http://127.0.0.1:8099/users/1
```

## Tests

```sh
cargo test
```

119 tests (92 integration + 14 CLI + 6 fuzz + 7 unit): const folding, CRUD, chained reads, comparison filters, params, body fields, error codes, JSON round-trip and escaping, query params, percent-decoding, protocol edge cases, `where` filters, persistence round-trip, status overrides, paging, list-cache invalidation, graceful shutdown, built-ins, CORS preflight, field projection, per-route metrics, indexed filters, password hashing, login and session flows, sorting, compile-error formatting, `Date` formatting, SHA-256, HMAC and PBKDF2 test vectors, header hardening, sort-cache, filter-cache and chain-cache invalidation, chain cache keys that must not collide, large-list caching across writes, request headers, guards, client-supplied ids, metrics, ETag round-trip, rate limiting, raw-socket HTTP (keep-alive, pipelining, HEAD, chunked rejection, split requests, 100 concurrent connections), concurrent writes, and a read/write stress test that hammers the list, sort, filter, search, and aggregate caches from five reader threads while four writers insert, then checks the final data is consistent.

`tests/cli.rs` drives the built binary end to end: `check` exit codes and error text, `new` refusing to overwrite, `openapi` output parsed back as JSON, a metrics endpoint, `include` across a directory of files, serving on a Unix socket, a program using every documented store operation and built-in, `--watch` restarting on a change to a route file or a folded-in asset and surviving a broken save, and a `POST` surviving a `SIGTERM` restart through the snapshot file.

`tests/fuzz.rs` adds six deterministic robustness tests: 2 000 mutated sources and 2 000 random byte strings through the compiler, 300 connections of malformed and truncated HTTP, 400 connections carrying byte-level mutations of otherwise valid requests (every answer must still be a well-formed status line), and oversized header and body requests. They also cover slow drip-feeding clients. `VELO_FUZZ_ROUNDS` raises the iteration counts for a longer hunt; 40 000 compiler mutations and 4 000 mutated requests have been run clean. They assert the process never panics and that the server still answers a normal request afterwards.

## CI

`./check.sh` is the gate: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, the full test suite, a release build, all three examples compiled, a performance guard against `bench/baseline.json`, and a boot smoke test with a short benchmark. `.github/workflows/ci.yml` runs the same steps on GitHub, but it is set to `workflow_dispatch` only: this account's Actions are blocked ("the job was not started because your account is locked due to a billing issue"), so every automatic run failed in three seconds without executing a step and marked good commits red. Once billing is settled, restore the trigger to run it on every push:

```yaml
on:
  push:
    branches: [main]
  pull_request:
```

Until then `./check.sh` is the real gate, and it is what every release here has passed.

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
| `src/lexer.rs` | tokens (internal) |
| `src/parser.rs` | source to routes, const folding, error messages |
| `src/ast.rs` | `Expr` tree, evaluation, built-in functions, request context |
| `src/store.rs` | collections, snapshots, JSON caches, persistence |
| `src/router.rs` | per-method exact map and param tree |
| `src/http.rs` | `Server`, dispatch, metrics, status codes |
| `src/serve.rs` | request parsing, connection state, the epoll loop (internal) |
| `src/socket.rs` | TCP and Unix listeners behind one type |
| `src/value.rs` | `Value`, JSON reader and writer |
| `src/date.rs` | `Date` header and ISO timestamp formatting (internal) |
| `src/crypto.rs` | SHA-256, HMAC, PBKDF2, password hashing, random bytes (internal) |
| `src/openapi.rs` | OpenAPI 3.0 document generation |
| `src/main.rs` | CLI |
| `examples/embed.rs` | using velo as a library |
| `src/bin/velobench.rs` | load generator |
| `src/bin/velomicro.rs` | in-process dispatch microbenchmark, `velomicro [rows]` |

## Build notes

`.cargo/config.toml` targets `x86_64-unknown-linux-musl` with `rust-lld`, so the build needs no system C toolchain. Incremental compilation is off there: it bought little on a project this size and its cache grew to gigabytes across many rebuilds. Remove that file to build against glibc with `cc`.

Requirements: Linux 4.5 or newer (the workers share the listener with `EPOLLEXCLUSIVE`), Rust 1.75 or newer, no crates.

## Changelog

**v1.9.0** — real auth. `password(text)` and `verify(text, stored)` hash and check a password with PBKDF2-HMAC-SHA256 and a fresh salt, `hash(text)` gives plain SHA-256, and `uuid()` now draws from a SHA-256 generator seeded from `/dev/urandom` instead of a 64-bit xorshift, so a session token cannot be predicted from one that was handed out earlier. Two bugs found while testing the flow: a cached chain result such as `where(...).count()` read as truthy in a guard whatever its value, so `when db.sessions.where("id", header.x_token).count()` admitted everyone; and a guard that hit an error answered with that error, so a login guard returned `404` for an unknown account and `401` for a wrong password, which told an attacker which accounts exist. Both are fixed. All of it is still zero dependencies.

**v1.8.0** — a chain that starts with `where(field, value)` now looks the value up in an index instead of scanning every row. The index is built on first use, appended to by inserts, and dropped by every other write, so it can never describe rows that have moved. Measured on 20 000 rows with a writer running flat out, which is the case where no cached answer survives long enough to be reused: a filtered count went from 859 to 2 445 req/s, a filter-sort-page-select chain from 2 283 to 3 758, and `first()` after a filter, the one shape that was never cached, from 87 to 3 288 req/s. The index cost 132 kB over those 20 000 rows.

**v1.7.0** — `/_metrics` gains a `paths` array: hits, failures, average and worst latency for each route that has served a request, labelled `METHOD /pattern`. The route a request matched now travels back out of `handle_full`, so a guard rejection or a store miss is charged to the route that caused it rather than only to the totals. Three relaxed atomics on a path that already reads the clock, so the cost stays inside the noise.

**v1.6.0** — `select(field, ...)` ends a chain by keeping only the fields it names, so a route can hide a column it must never return and shrink a list it does return. The projection is rendered once per version and cached like any other chain, keyed on the field names as well as the steps. Over 5 000 rows, `select("id", "name")` cut the body from 509 kB to 149 kB and lifted the route from 6 437 to 19 125 req/s.

**v1.5.0** — rows are stored in `Arc`-ed chunks of 128 instead of one flat vector. A read that misses the cache used to clone every row handle before rendering outside the lock, which cost four atomic operations a row and dominated the read path; it now clones one handle per chunk. An uncached filter over 20 000 rows fell from 2 346 us to 377 us, and a write that lands mid-read copies a single chunk rather than the whole collection. Under the mixed soak, with reads and writes both running flat out against 32 000 rows: the filter-sort-page chain went from 367 to 1 616 req/s, the filtered count from 375 to 1 352, the whole-collection page from 227 to 526, and inserts from 184 to 534 req/s, because readers stop hogging the machine.

**v1.4.2** — a cached read could go stale. A reader that missed the cache checked the collection version, then took the lock to store its result; a writer landing between those two steps cleared the cache and bumped the version, and the reader then wrote its now-outdated bytes into the cache it had just been invalidated out of. Every later request got the stale answer until the next write. The check and the store now happen under the same read lock, which a committing writer cannot hold. This affected every cached read shape since caching was introduced, not only chains. The stress test added in v1.4.1 caught it within minutes of being written, and now runs three rounds; against the unfixed build it fails about a third of the time on its own and more often under load.

**v1.4.1** — a stress test for chained reads: five readers on different chain shapes assert their own invariants (a page never exceeds its limit, never leaks another team's row, never comes back unsorted) while three writers insert and one deletes, then every chain is checked against the full list. A 150-second soak over HTTP with 24 reader connections and constant writes finished with zero errors and counts that add up, and memory tracked the data at 580 bytes a row.

**v1.4.0** — four builtins for the things every API does at its edges: `default(x, fallback)` for a missing or empty query parameter, and `lower`, `upper` and `trim` for normalising what a client sends. `db.users.page(default(query.offset, 0), default(query.limit, 20))` is now the whole of paging with sane defaults, and `create({ email: lower(trim(body.email)) })` normalises on the way in. All four fold at compile time when their arguments are constant.

**v1.3.0** — `query.x` and `header.x` no longer parse what they do not read. A route naming a field compiles to a direct lookup that scans the raw request for that one name and decodes only its value, instead of building an object out of every query pair and every header first. Pipelined throughput on a header-guarded route went from 234 000 to 644 000 req/s, on a two-field query route from 120 000 to 554 000, and a cached `where` driven by a query parameter fell from 1.21 us to 0.65 us. Routes that use `query` or `header` as a whole object still get one, unchanged.

**v1.2.0** — filters compare, not just match: `db.orders.where("amount", ">", 100)`, with `== != < <= > >=`, in a chain or on its own, and `first(field, op, value)` for the first row past a bound. Two steps make a range: `.where("score", ">=", query.lo).where("score", "<=", query.hi)`. The operator is a literal and unknown ones fail the compile; comparisons are numeric when both sides read as numbers and textual otherwise, and a row missing the field never matches.

**v1.1.0** — read operations chain: `db.users.where("team", query.t).order("-score").page(0, 20)`, ending in a list, `count()`, an aggregate, or `first()`. A chain is cached as one result keyed on every step, so it costs about what a single filter costs and stays flat as the collection grows. A row's field can now be read straight off a call, as in `db.users.find(id).name`. Separately, inserting into a large collection no longer throws the cached list away: `VELO_APPEND_MAX` now only limits the copy a concurrent reader forces, so a 20 000-row collection under write-then-read load went from 3 200 us to 57 us per cycle.

**v1.0.0** — the surface is settled and the engine has been measured, soaked, and fuzzed, so this is the first version promising not to break what is documented above. The language (routes, params, query, headers, body, arithmetic, comparisons, guards, includes, `file()`, `openapi()`), the store (CRUD, filters, search, sort, paging, aggregates, bulk deletes, snapshots), and the runtime knobs are stable; anything added from here is additive. Ninety tests, a deterministic fuzz suite, a performance guard, and a Go baseline for scale.


**v0.53.1** — the event loop, lexer, epoll wrapper, and date formatter are now crate-private; the library surface is the language, the store, the server, and the values that flow through them.

**v0.53.0** — `examples/embed.rs` shows velo used as a library: compile against your own store, seed it, dispatch without a socket, then serve. `Value::object(&[("k", v)])` makes building rows from Rust readable, and `check.sh` runs the example.

**v0.52.2** — incremental compilation disabled in `.cargo/config.toml`; its cache had grown to 7.6 GB over the project's rebuild history.

**v0.52.1** — `--watch` also follows the files that `file()` folds in, so editing a served page reloads it.

**v0.52.0** — a Scope section states what velo is for and what it deliberately is not, including the protocol behaviour verified against a raw socket: HTTP/1.0 fallback, origin-form targets only, 411 for chunked bodies, case-sensitive methods.

**v0.51.1** — snapshots are written from a read lock and skip tombstones directly, so autosave no longer compacts the collection or holds a row reference while it writes. `bench/baseline.json` refreshed against the current build.

**v0.51.0** — readers copy the row handles they need under a brief lock instead of holding an `Arc` for the whole render, so an insert never triggers a copy-on-write of a large row list: writes under a heavy reader went from 15 500-21 600 to 32 900-33 500 req/s and the worst insert from 1 150 ms to 49 ms.

**v0.50.1** — a test now compiles a program that exercises every documented store operation and built-in, and checks that every `VELO_*` knob named in this file exists in the code. Documentation cannot drift silently.

**v0.50.0** — derived results (filters, searches, sorts, aggregates) are rendered from an `Arc` snapshot taken under a brief read lock rather than while holding it. A reader repeatedly rebuilding a 186 000-row query no longer starves writers: inserts went from 779-1 091 req/s back to 15 000-21 000 req/s.

**v0.49.0** — a soak run exposed a pathology: with one reader on a 226 000-row list, every insert copied the whole cached list, dropping writes from 41 400 to 154 req/s. Inserts now stop extending a list past `VELO_APPEND_MAX`, and list rendering happens under a read lock, so the same test runs at 15 000 req/s with p50 back to 0.29 ms.

**v0.48.0** — `date(ms)` renders an instant as an ISO-8601 UTC string, so `date(now())` gives a timestamp a human can read; the todo example stores one.

**v0.47.1** — the interner also covers the field names velo generates itself (`id`, `value`, `deleted`) and uses the fast hasher, so creating a row no longer allocates its key strings.

**v0.47.0** — JSON object keys are interned per worker, so a body or snapshot that repeats the same field names allocates each name once: loading 270 000 rows dropped from 204 MB to 168 MB of RSS.

**v0.46.0** — constant routes (including `file()` and `openapi()`) carry an `ETag` computed at compile time instead of hashing the body on every request, and `velo new` writes a starter that shows search, validation, upsert, and a served OpenAPI document.

### Earlier

- **v0.45.0** — bulk deletes: `db.x.delete_where(field, value)` and `db.x.clear()`, both answering with how many rows went away.
- **v0.44.0** — `db.x.upsert(key, value)` merges into an existing row or creates one under that key, which is what `PUT /users/:id` usually means.
- **v0.43.1** — every benchmark in this file re-measured against the current build, and `velobench` stopped scanning each response head for `Connection: close` (that….
- **v0.43.0** — systemd socket activation: when `LISTEN_FDS` names an inherited listener, velo serves on it instead of binding its own.
- **v0.42.0** — shutdown drains instead of dropping: no new connections, in-flight requests answered with `Connection: close`, exit when the last one finishes or….
- **v0.41.1** — `velobench` can drive a Unix socket, which measured the gain: 86.9k to 124.4k req/s on `/health` compared with loopback TCP.
- **v0.41.0** — `velo run app.velo unix:/run/velo.sock` listens on a Unix socket: stale files are replaced, permissions come from `VELO_SOCKET_MODE`, and the socket….
- **v0.40.0** — compile errors now report a column and underline the offending token with a caret; the lexer tracks column positions for every token.
- **v0.39.0** — `velomicro --json` and `velomicro --check <baseline>`: the microbenchmark can now fail the build when an operation regresses past a multiple of the….
- **v0.38.0** — conditions combine with short-circuiting `and` / `or`.
- **v0.37.0** — expressions gained `+ - * /`, `< > <= >=`, and parentheses, with the usual precedence.
- **v0.36.1** — `velo routes` prints where each route came from, which matters once files include each other; dropped an unused store method and fixed the quoting in….
- **v0.36.0** — `deploy/` templates: a hardened systemd unit, a Caddy reverse-proxy block that forwards the client IP, and a cloudflared ingress entry.
- **v0.35.1** — benchmarked against a Go `net/http` server on the same box for scale: 1.8x the throughput on small responses, 14x pipelined, a tenth of the memory.
- **v0.35.0** — `VELO_HEADERS` adds fixed response headers (security or caching policy), rejecting malformed entries and anything carrying control characters.
- **v0.34.0** — request logs now carry the response size and duration, and `VELO_LOG=json` emits one JSON object per line.
- **v0.33.0** — `file("page.html")` folds a file into a route at compile time and serves it with a content type derived from its extension; response content types….
- **v0.32.1** — fuzz iteration counts are configurable with `VELO_FUZZ_ROUNDS`; an extended run (40 000 compiler mutations, 4 000 mutated requests) found only a….
- **v0.32.0** — a request body that is not JSON is retried as `application/x-www-form-urlencoded`, so HTML forms and `curl --data-urlencode` work without changing a….
- **v0.31.0** — rendered bytes are held as `Arc<Vec<u8>>`, so building a cache entry no longer copies it and an insert can extend the cached list in place.
- **v0.30.1** — the header terminator scan resumes from where it left off instead of restarting on every read, so drip-fed headers cost linear work.
- **v0.30.0** — `velo run --watch` restarts the server when the route file or any included file changes, stopping the old process with `SIGTERM` so snapshots flush,….
- **v0.29.1** — `openapi()` and `velo openapi` now describe included files too; the document is built after the merge.
- **v0.29.0** — `include "other.velo"` merges route files, resolved relative to the including file, with repeats skipped so cycles are harmless.
- **v0.28.0** — deleting is now constant time: rows are tombstoned instead of shifted, and the collection compacts when tombstones exceed half the rows.
- **v0.27.1** — measured the store at 20 000 rows and published the numbers: `find` and `create` stay flat, list-shaped operations are bound by the bytes they copy,….
- **v0.27.0** — inserting a row now appends to the cached list JSON when the list has been read since the last write, cutting a write-then-list cycle from 309 us to….
- **v0.26.2** — README reordered into a reading path with a table of contents.
- **v0.26.1** — fuzz suite now mutates valid requests byte by byte and asserts every answer is still a well-formed HTTP status line.
- **v0.26.0** — metrics now report `bytes_out`, `avg_micros`, and `max_micros`, measured only when `VELO_METRICS` is set.
- **v0.25.1** — the per-worker cache view is bounded by bytes as well as entries, so invalidated large results are released instead of being held by every worker….
- **v0.25.0** — two evaluation changes: object and array routes render straight into the output buffer instead of building a `Value` tree (`/stats` 417k to 785k….
- **v0.24.0** — aggregations: `sum`, `avg`, `min`, `max` over a numeric field, cached and invalidated with the other derived results.
- **v0.23.1** — stress test covering cache invalidation under concurrent reads and writes.
- **v0.23.0** — `db.x.search(field, text)`: case-insensitive substring match over a field, cached and invalidated like `where` and `order`.
- **v0.22.0** — building on a non-Linux target now fails with a plain message instead of a link error, and the platform requirements are stated up front.
- **v0.21.2** — bounded response buffering: pipelined requests stop being rendered past 256 kB of pending output and resume after the flush. 100 pipelined 16 kB….
- **v0.21.1** — end-to-end CLI tests: the real binary is started, driven over TCP, stopped with `SIGTERM`/`SIGINT`, and restarted to prove the snapshot round-trips.
- **v0.21.0** — `openapi()` built-in: a route can serve this API's own OpenAPI document, folded to constant bytes at compile time.
- **v0.20.0** — per-client rate limiting: `VELO_RATE` requests per second, keyed on the socket address or on `VELO_REAL_IP_HEADER` behind a proxy, answered with 429.
- **v0.19.1** — snapshot interval is now self-tuning: a save may cost at most `VELO_SAVE_DUTY` percent of wall time (10 by default).
- **v0.19.0** — `velo openapi` generates an OpenAPI 3.0 document from the compiled routes.
- **v0.18.2** — the render caches now respect a byte budget (`VELO_CACHE_BYTES`, 8 MB by default) instead of only an entry count, so 32 large lists cannot quietly….
- **v0.18.1** — `where` results are cached per field and value alongside the sorted and full-list caches, all cleared on any write (`where` 77 us to 1.2 us per call….
- **v0.18.0** — `when <condition> or <status>` picks the status a failed guard answers, so a guard doubles as body validation (`when body.name else 400`).
- **v0.17.1** — `where` and `first` compare fields without allocating a string per row and `order` extracts each sort key once: `where` over HTTP went from 2.4k to….
- **v0.17.0** — optional `ETag` / `If-None-Match` (`VELO_ETAG=1`): 200 `GET` and `HEAD` responses carry an FNV tag of the body and a matching conditional request….
- **v0.16.2** — `Expect: 100-continue` is answered with an interim `100 Continue` instead of leaving the client to time out before sending its body.
- **v0.16.1** — split the two large modules: evaluation moved out of `parser.rs` into `ast.rs`, the event loop out of `http.rs` into `serve.rs`.
- **v0.16.0** — optional metrics endpoint (`VELO_METRICS=/_metrics`) reporting version, uptime, requests, failures, live connections, routes, and workers.
- **v0.15.1** — connections that never complete a request are dropped after `VELO_HEADER_TIMEOUT` (10s) measured from accept, so drip-feeding headers cannot hold a….
- **v0.15.0** — `db.x.first(field, value)`, `create` honours an `id` supplied in the body (409 on duplicates), `velo new` writes a starter file,….
- **v0.14.1** — repository hygiene: `rustfmt.toml`, formatted tree, zero clippy warnings, and a GitHub Actions workflow running fmt, clippy, tests, release build,….
- **v0.14.0** — route guards: `when <condition>` with `==` / `!=` or a truthiness check, answering 401 before the body runs.
- **v0.13.0** — request headers in expressions: `header.x_team` (lowercased, hyphens as underscores), parsed only for routes that mention `header`.
- **v0.12.1** — deterministic fuzz suite for the compiler and the HTTP parser.
- **v0.12.0** — `order(...)` results are cached per sort key and invalidated on write (6k to 120k req/s pipelined), and `find`/`update`/`delete` on a plain path….
- **v0.11.0** — spec and hardening pass: `Date` response header (computed once per second per worker), 400 on conflicting `Content-Length` headers, backoff instead….
- **v0.10.0** — `db.x.order(field)` sorting (`"-field"` descending, numbers compare numerically) and compile errors that print the offending source line.
- **v0.9.0** — `VELO_CORS` adds the allow-origin headers and answers `OPTIONS` preflight with a bodyless 204; `VELO_LOG` prints one line per request.
- **v0.8.0** — built-in functions: `now()`, `uuid()` (v4, seeded from `/dev/urandom`, per-thread xorshift after that), `len(x)`, `env("NAME")`.
- **v0.7.1** — graceful shutdown: `SIGINT`/`SIGTERM` unwind the event loop and flush a final snapshot when `--data` is set.
- **v0.7.0** — rows carry their rendered JSON and collections cache the JSON of the whole list, invalidated on write.
- **v0.6.0** — per-route status override (`expr : 204`, bodyless 204/304 responses) and `db.x.page(offset, limit)` for pagination.
- **v0.5.0** — optional persistence: `--data file.json` loads at boot and autosaves on change (atomic rename, dirty-flag gated, `VELO_SAVE_MS`).
- **v0.4.0** — query strings (`query.name`), percent-decoding for path params and query values, `db.x.where(field, value)` filters.
- **v0.3.0** — epoll event loop replaces thread-per-connection: one epoll per core sharing the listener with `EPOLLEXCLUSIVE`, non-blocking sockets, EPOLLOUT-driven….
- **v0.2.1** — `velobench` load generator (thread per connection, pipelining, p50/p99), explicit `Connection: keep-alive` response header so standard tools reuse….
- **v0.2.0** — rewritten in Rust, zero dependencies.
- **v0.1.0** — first version (Go): language, closure compiler, router, in-memory store.
