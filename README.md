# Velo

**v0.1.0** — a tiny language for HTTP APIs. One line per endpoint, compiled to closures, served by a zero-allocation router.

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
go build -o bin/velo ./cmd/velo
./bin/velo run examples/api.velo :8080

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

Methods: `GET POST PUT PATCH DELETE HEAD OPTIONS`. Comments: `#` or `//`.

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

| op | returns | status |
| --- | --- | --- |
| `all()` | every row | 200 |
| `find(id)` | one row | 200, or 404 |
| `create(value)` | the created row with its `id` | 201 |
| `update(id, patch)` | merged row | 200, or 404 |
| `delete(id)` | `{"deleted":true}` | 200, or 404 |
| `count()` | number of rows | 200 |

`POST` routes answer `201` by default, everything else `200`. Unknown path is `404`, known path with the wrong method is `405`, malformed JSON body is `400`, body over 1 MB is `413`.

## Design

- **Compile to closures.** Parsing produces a tree of `func(*Ctx) (Value, *Err)` closures, not an AST that gets walked per request. No type switches on the hot path.
- **Compile-time resolution.** Path parameters become slot indexes, collections become pointers. Nothing is looked up by name at request time.
- **Constant folding.** A route whose expression touches no request state (`GET /health => "ok"`) is serialized once at compile time and the bytes are written straight to the socket.
- **Zero-allocation router.** Static paths hit a per-method map, dynamic paths walk a segment trie, parameters land in a fixed array inside a pooled `Ctx`.
- **Hand-written JSON.** No reflection, no `encoding/json`. Encoding appends into pooled buffers; objects are ordered key/value slices, so output is deterministic and small records stay cache-friendly.
- **Copy-on-write store.** Readers load an immutable snapshot pointer with no lock at all; writers copy under a mutex and swap. API traffic is read-heavy, so reads pay nothing.

## Benchmarks

`go test -bench . -benchmem` (Xeon Gold 6140, 4 cores, Go 1.26):

| benchmark | ns/op | allocs/op |
| --- | --- | --- |
| `RouterStatic` | 26.7 | 0 |
| `RouterParam` | 93.8 | 0 |
| `JSONEncodeRow` | 142 | 0 |
| `JSONParseRow` | 1078 | 17 |
| `Compile` (12 routes) | 9878 | 84 |

End-to-end over TCP with the bundled load generator, server and generator sharing the same 4 cores:

```sh
go build -o bin/velobench ./cmd/velobench
./bin/velobench -c 64 -d 5s http://127.0.0.1:8080/health
```

| route | rps | p50 | p99 |
| --- | --- | --- | --- |
| `/health` (const) | 29.8k | 1.12 ms | 13.7 ms |
| `/users/:id` (store find) | 28.0k | 1.16 ms | 14.7 ms |
| `/users` (list) | 28.9k | 1.16 ms | 14.2 ms |

Resident memory under load: **10 MB**. Numbers are CPU-bound on a shared 4-core box; the generator competes with the server for cores.

## Testing

```sh
go test ./...
go test -bench . -benchmem
```

Covers routing, all store operations, JSON round-trips, error statuses, concurrent readers and writers, and compile-error cases.

## Known limits (v0.1.0)

- Store is in-memory only, nothing is persisted across restarts.
- `delete` rebuilds the id index, so it is O(n) in collection size.
- No query-string access, no filtering, no operators or arithmetic in expressions.
- No middleware, auth, or CORS.
- `-race` is unavailable on the dev box (no cgo toolchain); concurrency is covered by a stress test instead.

## Roadmap

- v0.2: query parameters, filtering and pagination, response status control
- v0.3: persistence, faster HTTP write path, connection-level tuning
- v0.4: middleware, validation, standard library functions

## Changelog

### v0.1.0
- Lexer, parser and closure compiler for the route DSL
- Zero-allocation router with static map plus dynamic segment trie
- In-memory copy-on-write store with `all/find/create/update/delete/count`
- Hand-written JSON encoder and parser
- Constant folding for request-independent routes
- CLI: `run`, `check`, `routes`, `version`
- `velobench` load generator with latency percentiles
- Test suite and micro-benchmarks
