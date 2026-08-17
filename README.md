# Velo

**v1.50.0** — a tiny language for HTTP APIs, written in Rust with zero dependencies. One line per endpoint, compiled to an expression tree, served by an epoll event loop.

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

[Scope](#scope) · [Quick start](#quick-start) · [Language](#language) · [Guards](#guards) · [OpenAPI](#openapi) · [Embedding](#embedding) · [Persistence](#persistence) · [Concurrency](#concurrency) · [Rate limiting](#rate-limiting) · [Metrics](#metrics) · [Deployment](#deployment) · [Design](#design) · [Benchmarks](#benchmarks) · [Tests](#tests) · [CI](#ci) · [Layout](#layout) · [Build notes](#build-notes) · [Releases](#releases)

## Scope

Velo suits a JSON API whose data fits in memory on one machine: an internal service, a mobile or web backend, a prototype that has to be fast, a sidecar that shapes data for something else. It compiles a fixed set of routes and precomputes as much of each response as it can.

What a whole API needs is in the language rather than around it: CRUD, filters, sorting, paging and aggregates; password hashing and session lookups; cookies; per-key rate limits; per-condition validation with the reason the client is told; row expiry; atomic counters; field allowlists so a row never leaves with more than it should. A generated OpenAPI document, per-route metrics, request logs, `ETag`, CORS and a draining shutdown come with the server, not with a stack around it.

It is deliberately not: a database (the store is memory-bound, single-process, snapshot-persisted), a TLS terminator, an HTTP/2 or WebSocket server, a file-upload endpoint, or a cluster. It has no joins, no transactions across two collections, and no user-defined functions: a request either fits one collection and one expression or belongs somewhere else. Put a proxy in front for TLS, compression, and HTTP/2, and keep the dataset within RAM.

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
| `velo run <file> [addr] [--data f.json] [--wal f.log] [--watch]` | start the server; `addr` is `:8080`, `127.0.0.1:8080`, or `unix:/run/velo.sock` |
| `velo check <file>` | compile only, report errors with the offending line, column, and a caret |
| `velo routes <file>` | list compiled routes with their kind, status, guard, and source file and line |
| `velo bench <file> [-c n] [-d secs] [-H hdr] [-b body] [-q query] [--data f.json]` | serve the file and load every route it can drive, slowest first; `-d` takes fractions of a second, `-H` is repeatable, `-b` and `-q` set the body and query string |
| `velo openapi <file>` | print an OpenAPI 3.0 document for the routes |
| `velo new <file>` | write a starter file |
| `velo version` | print version |
| `velobench [-c n] [-d secs] [-p depth] [-m method] [-b body] [-H hdr] <url>` | built-in keep-alive load generator; `unix:/sock//path` targets a Unix socket |
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

Naming a directory takes every `.velo` file directly inside it, in name order, which keeps one line in the entry file however many parts there are:

```velo
include "parts"

GET /health => "ok"
```

It does not descend into subdirectories, and anything that is not a `.velo` file is left alone, so notes and templates can live beside the routes. `--watch` follows the directory itself, so a part added while the server is running is picked up like an edit to an existing one. `examples/shop/app.velo` uses this form.

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
| request cookie | `cookie.session` | one cookie by name, read straight from the `Cookie` header; `null` when it is not there |
| store call | `db.users.find(id)` | see below |
| projection | `body.select("name", "email")` | the object with only those fields, applied to each element of an array |
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
| `where(field, "in", list)` | rows whose `field` is any of a comma-separated list or a JSON array | `[]` |
| chained steps | `where`, `search`, `order` and `page` compose; see below | |
| `page(offset, limit)` | slice of rows, `limit` 0 means "to the end" | `[]` |
| `search(field, text)` | rows whose `field` contains `text`, case-insensitive | `[]` |
| `order(field)` | rows sorted by `field`, `"-field"` for descending | `[]` |
| `create(value)` | the stored row; `id` is generated unless the value carries one | 400 on an empty body, 409 on a duplicate `id` |
| `create(value, field, ...)` | same, refusing a value whose `field` another row already holds | 409 on a duplicate |
| `update(key, patch)` | merged row; a nested object merges too | 404 |
| `update(key, patch, field, ...)` | same, refusing a patch that would take a `field` another row holds | 404, 409 on a duplicate |
| `incr(key, field)` | the row with `field` one higher, atomically; `incr(key, field, n)` to step by `n` | 404, 409 if the field holds something other than a number |
| `upsert(key, value)` | merged row, or a new row keyed by `key` | never misses |
| `upsert(key, value, field, ...)` | same, refusing a value that would take a `field` another row holds | 409 on a duplicate |
| `delete(key)` | `{"deleted":true}` | 404 |
| `delete_where(field, value)` | `{"deleted":n}` | `{"deleted":0}` |
| `delete_where(field, op, value)` | same, with `op` one of `== != < <= > >=`, `"in"` included | `{"deleted":0}` |
| `clear()` | `{"deleted":n}`, resets generated ids | `{"deleted":0}` |
| `select(field, ...)` | the rows with only those fields kept, or one row after `find`/`first`; a name may be a path like `"profile.name"` | `[]` / 404 |
| `group(field).count()` | `{value: rows}` for each distinct value of `field` | `{}` |
| `group(field).sum(f)` | same shape, holding the aggregate of `f` per group; `avg`, `min` and `max` too | `{}` |

A counter cannot be written as a read and a write. `db.posts.update(id, { views: db.posts.find(id).views + 1 })` reads one snapshot and writes into another, so two requests that land together both read the same number and one of the two increments is lost. `incr` does the whole step inside the collection's write lock:

```velo
POST /posts/:id/view => db.posts.incr(id, "views") : 200
POST /posts/:id/vote => db.posts.incr(id, "score", body.by) : 200
```

Eight threads calling the first route 250 times each leave `views` at exactly 2 000. A missing field starts at the step rather than failing, so a row never has to be created with its counters already in it; a negative step counts down; a field holding anything but a number answers `409 {"error":"not a number"}` and changes nothing, and `id` cannot be stepped because it is the key.

A patch merges as deep as the object goes. `db.users.update(id, { profile: { city: "cnx" } })` changes that one field and leaves the rest of `profile` alone, which is what a client sending a `PATCH` means and what velo used to get wrong: it replaced the whole nested object, so a request that looked like it set one field quietly threw away every other field beside it. A list is replaced rather than merged, since merging two lists has no obvious answer. Writing a plain value over an object replaces it, which is how a nested object is cleared out and rebuilt: patch it to `""` and then patch the object you want. `upsert` merges the same way, so one rule covers the language rather than two.

A second row with the same email is the same shape of bug. `db.users.create(body) when db.users.where("email", body.email).count() == 0 else 409` reads one snapshot and writes into another, so two signups landing together both find nothing and both succeed. Naming the field on `create` moves the check inside the write lock:

```velo
POST /signup => db.users.create({ id: uuid(), email: lower(body.email), pass: password(body.pass) }, "email")
```

Eight threads racing two hundred rounds, all eight sending the same email each round, leave exactly two hundred rows. Any number of fields can be named and each is checked separately, so `create(body, "email", "phone")` refuses a row that collides on either. A field the new row does not carry is not compared, since otherwise only one row could ever be stored without it; require it with `check(body.email, "email is required")`. `update` and `upsert` take the same fields, so a route that lets a client change an email keeps the guarantee: `db.users.upsert(id, body, "email")` refuses a value another row already holds and answers 409, while writing a row's own email back to itself is not a collision. A collection large enough to be indexed uses that index for the check, so it costs a lookup rather than a scan.

`in` is what a batch endpoint needs, so a client fetching twenty rows asks once rather than twenty times:

```velo
GET  /users/batch => db.users.where("id", "in", query.ids)
POST /users/batch => db.users.where("id", "in", body.ids).select("id", "name")
```

`GET /users/batch?ids=7,19,204` reads the list from the query, spaces around a value are trimmed, and a JSON array works the same way, so a POST can send `{"ids":[7,19,204]}`. Each value is looked up in the equality index and the results are merged, which is what makes it worth having: on 4 500 rows, three ids cost 3.4 us against 2 141 us for the same query with the index turned off.

Read operations chain, so one line can filter, sort and page:

```velo
GET /users/top   => db.users.where("team", query.team).order("-score").page(0, 20)
GET /users/band  => db.users.where("score", ">=", query.lo).where("score", "<=", query.hi).order("score")
GET /users/hits  => db.users.search("name", query.q).count()
GET /users/spend => db.users.where("team", query.team).sum("score")
GET /users/best  => db.users.where("team", query.team).order("-score").first()
GET /users/cards => db.users.where("team", query.team).select("id", "name")
```

`group` turns a chain into one object keyed by a field's distinct values, which is the shape a dashboard wants:

```velo
GET /users/by/team    => db.users.group("team").count()
GET /users/spend/team => db.users.group("team").sum("score")
GET /orders/today     => db.orders.where("at", ">", now() - 86400000).group("status").count()
```

`{"blue":12,"red":5}` rather than one request per team. Keys come back sorted, so the same data always renders the same bytes and an `ETag` on the route means something. A row whose field is missing or `null` belongs to no group. `count` counts rows; `sum`, `avg`, `min` and `max` take a second field and follow the same rules they do on their own, so a group with no numeric values sums to `0` and averages to `null`. Every step that can come before it still can: filter, search and order first, group last. A bare `group(field).count()` over a whole collection is answered from the equality index, which already holds each value's rows, so it costs the number of groups rather than the number of rows: 5.7 us on 6 500 rows against 232 us for the walk it replaced. It falls back to the walk whenever the index cannot answer honestly, which is when a step comes first, when an aggregate needs the values themselves, below the 512 rows an index needs, or when any row is missing the field, since the index cannot tell a missing value from an empty one and `group` must. The walk is cached like any other read and rebuilt on the next write.

Every field name in a read may be a path, so a nested row is queried the way a flat one is:

```velo
GET /orders/mine   => db.orders.where("customer.id", header.x_customer)
GET /orders/big    => db.orders.where("total.amount", ">", query.min).order("customer.name")
GET /orders/cities => db.orders.group("customer.city").sum("total.amount")
```

`where`, `order`, `search`, `first`, `group`, the aggregates and `select` all read a path the same way, because a rule that holds for one and not the others is worse than one that holds nowhere. A path is tried only when the flat lookup misses, so a field that is simply a field costs exactly what it did before; the equality index carries a path like any other name, which a test proves on 700 rows. A row that does not have the path is treated as a row that does not have the field.

A field name can name a path, which is what stops a nested object carrying more than it should:

```velo
GET /users/:id    => db.users.find(id).select("id", "profile.name", "profile.city")
GET /orders/:id   => db.orders.find(id).select("id", "lines.item", "lines.qty")
```

The answer keeps the shape it had, `{"id":1,"profile":{"name":"mark","city":"bkk"}}`, so a client reads it the way it read the whole row. Naming a parent keeps all of it, so `select("profile", "profile.name")` is the same as `select("profile")`: the wider of the two wins rather than the narrower, which is the safe way round for a rule about what to keep. Paths go as deep as the data does. A path through a list applies to every element, so `lines.item` keeps that field of every line. A path whose parent is missing, or whose parent is not an object, leaves nothing behind rather than a `null`.

`select` also follows `find` and `first`, which is how a route returns one row without the fields a client must not see:

```velo
GET /users/:id   => db.users.find(id).select("id", "name", "team")
GET /users/best  => db.users.order("-score").first().select("name", "score")
GET /users/byname => db.users.first("email", query.q).select("id", "name")
```

A projected miss is still a 404, a field the row does not have is skipped, and the fields come back in the order the route names them.

`select` ends a chain by narrowing each row to the fields it names, in the order it names them, skipping any the row does not have. It is how a route keeps a password hash or an internal note out of the response, and it is the cheapest way to shrink a large list: over 5 000 rows, trading five fields for two took the body from 509 kB to 149 kB and the route from 6 437 to 19 125 req/s, all of it saved on the wire.

```velo
GET /users        => db.users.select("id", "name", "team")
GET /users/public => db.users.where("active", true).order("name").select("id", "name")
```

A filter compares with `==` by default, and that is the case the collection indexes, so `where("team", query.team)` stays cheap as the collection grows even while it is being written to. Several `==` filters in one chain are all answered from their indexes and intersected, so the narrowest one decides the work: `where("team", t).where("email", e)` reads the handful of rows in both lists rather than every row on the team. A `page` stops that, because paging is positional and the rows it keeps depend on what ran before it. Pass an operator to widen it:

```velo
GET /orders/big  => db.orders.where("amount", ">", 100)
GET /orders/open => db.orders.where("status", "!=", "done").count()
```

The operator is a literal, checked at compile time. A comparison is numeric when both sides read as numbers, otherwise it compares text; a row missing the field never matches. `where`, `search`, `order` and `page` are steps and may repeat in any order. `count()`, `sum/avg/min/max(field)`, `select(field, ...)` and `first()` end a chain and nothing may follow them; `first()` answers 404 when the chain is empty. A chained result is cached like a single call, keyed on the whole chain, and thrown away when the collection changes. `first()` is the one shape that is not cached: it scans instead, and a trailing `order` picks the extreme row in one pass rather than sorting. An `order` followed by a `page` only sorts as far as that page reaches, so a top-twenty costs a pass over the rows and a sort of twenty rather than a sort of all of them.

`select` is not only a store operation. It narrows any object, which is how a write route decides what a client is allowed to set:

```velo
POST /users => db.users.create(body.select("name", "email")) when body.name else 400
```

Without it, `create(body)` stores whatever the client sent, so a request carrying `"role":"admin"` or its own `"pass"` field writes those too. With it, everything outside the list is dropped before the row exists. On an array it narrows each element, on anything that is not an object it is `null`, and it never reaches deeper than the level it is applied to. `select()` with no fields is a compile error everywhere it can appear.

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
| `setcookie(name, x)` | sets a hardened `Set-Cookie` on the response and returns `x` |
| `limit(key, per_second)` | `true` while that key is under its rate, `429` once it is not |
| `check(condition, reason)` | `true` while the condition holds, `400` with that reason once it does not |
| `openapi()` | this API's OpenAPI 3.0 document, rendered once at compile time |
| `file("page.html")` | the file's contents, read at compile time, served with a content type from its extension |

Everything but `now()`, `uuid()`, `date()`, `password()`, `verify()`, `setcookie()`, `limit()`, `check()` and `openapi()` is folded at compile time when its arguments are constant, so `upper("velo")` costs nothing at runtime.

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

`VELO_RATE` caps every client at one budget for the whole API, which is the wrong shape for a login: verifying a password costs a full PBKDF2, and an attacker working through one account from many addresses never trips a per-address counter. `limit(key, per_second)` puts a ceiling on whatever you name:

```velo
POST /login => db.sessions.create({ id: setcookie("session", uuid()), user: lower(body.email) }) when limit("login:" + lower(body.email), 5) and verify(body.pass, db.users.find(lower(body.email)).pass) else 401
```

Over the ceiling answers `429 {"error":"too many requests"}` whatever `else` says, because a client that is being throttled should be told to back off rather than told its password was wrong. Under the ceiling the guard carries on, so a wrong password is still a `401`. Put `limit` first in the guard: `and` short-circuits, so anything to its left decides whether the attempt is counted at all, and a failed attempt must count. The key is any expression, so `limit(header.x_real_ip, 100)` throttles an address, `limit("login:" + body.email, 5)` throttles an account across every address, and naming both costs two calls. Buckets are fixed one-second windows shared by the whole process, so two routes naming the same key share one budget.

In a browser, hand the token out as a cookie instead of a header. `setcookie(name, value)` writes the `Set-Cookie` and returns `value`, so the same expression that generates a token both stores it and sends it:

```velo
POST /login  => db.sessions.create({ id: setcookie("session", uuid()), user: lower(body.email) }).user when verify(body.pass, db.users.find(lower(body.email)).pass) else 401
GET  /me     => db.sessions.find(cookie.session).user when db.sessions.where("id", cookie.session).count()
DELETE /logout => [db.sessions.delete(cookie.session), setcookie("session", "")] : 204 when db.sessions.where("id", cookie.session).count()
```

Every cookie is written `Path=/; HttpOnly; SameSite=Lax`, and `VELO_COOKIE_SECURE=1` adds `Secure` for a TLS proxy. There are no per-call options: a cookie velo writes cannot be read by JavaScript and cannot be sent cross-site. An empty value expires it, which is how logout clears one. A name or value velo cannot write verbatim, because it holds a space, a semicolon, a quote or a control character, sets no cookie at all rather than a mangled one, so a token can never carry a forged header or a smuggled `Domain` into the response. An array evaluates left to right, which is how logout deletes the row and clears the cookie in one route.

`examples/auth.velo` accepts the token from either place with `default(cookie.session, header.x_token)`, so the same API serves a browser and a script.

Keying a user on the lowercased email is what makes signup safe against a second attempt with the same address: an `id` collision is refused inside the write lock. When the key has to be a `uuid()` instead, name the field on `create` and it is checked the same way: `db.users.create({ id: uuid(), email: lower(body.email), pass: password(body.pass) }, "email")`.

Sessions are rows, and rows do not remove themselves. `VELO_EXPIRE=sessions.until` sweeps a collection every minute and deletes every row whose named field holds a Unix time in milliseconds that has passed, which is exactly the `until` a login writes. A row without that field, or with something that is not a number in it, is never swept. `VELO_EXPIRE_MS` changes how often, and several collections can be listed at once:

```sh
VELO_EXPIRE=sessions.until,resets.expires velo run app.velo :8080
```

The same thing can be done from a route, which is useful when the cadence should be yours:

```velo
DELETE /sessions/stale => db.sessions.delete_where("until", "<", now()) : 200
```

A session is a row like any other, so a token is checked with a `where(...).count()` guard, which the equality index answers without a scan, and expiry is a comparison against `now()`. `uuid()` draws from a SHA-256 generator seeded from `/dev/urandom`, so a token cannot be guessed from earlier ones. Never end a route on the row that holds the digest: `select` the fields a client may see. That works on one row as well as a list, so `db.users.find(id).select("id", "email")` is the safe way to serve a profile.

`hash(x)` is plain SHA-256 in lowercase hex, for fingerprints, cache keys, or storing an API key you only ever compare. It is not a password hash; use `password()` for those. With a constant argument it folds at compile time.

`examples/auth.velo` is a complete signup, login, session and logout API. A reason can follow the status, and it becomes the error body instead of the generic one, so a client is told what to fix rather than only that something was wrong:

```velo
POST /users  => db.users.create(body.select("name", "email")) when body.name else 400 "name is required"
POST /orders => db.orders.create(body) when body.qty > 0 and body.qty < 100 else 400 "qty must be between 1 and 99"
GET  /admin  => db.audit.all() when header.authorization == env("ADMIN_TOKEN") else 403 "admin token required"
```

`velo openapi` uses the reason as the description of that status code, so the document says the same thing the server does. Without one the body stays `{"error":"unauthorized"}`, or `{"error":"invalid body"}` for a `400`.

One `else` carries one reason, which is not enough when a route checks several things. `check(condition, reason)` gives each condition its own, and `and` stops at the first that fails, so the client is told what to fix one thing at a time:

```velo
POST /users => db.users.create(body.select("name", "email", "age"))
  when check(body.name, "name is required")
   and check(body.email, "email is required")
   and check(body.age > 0, "age must be positive")
   and check(len(body.name) < 20, "name must be under 20 characters")
```

A failed `check` always answers `400` with its reason, whatever `else` says, the way a failed `limit` always answers `429`. It mixes with the guards that were already there, so `when header.x_key and check(query.n, "n is required")` still answers `401` for the missing header and `400` for the missing query. Use it with `and`: an `or` reports the left-hand reason rather than trying the right-hand side, because a failed check refuses immediately.

`examples/todo.velo` is a complete todo API using uuid keys, timestamps, sorting, and filters. `examples/shop/` splits a larger API over four files with `include`: a catalog with search, orders keyed by a customer header, and an admin section behind a token guard.

## OpenAPI

What is left over is listed with the reason: a route that needs more than one path parameter, one whose collection holds no row to take an id from, and one whose body cannot be built without `-b`.

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

The store is in memory, so a collection is bounded by RAM and a snapshot is the whole dataset. Budget roughly 600 bytes of RAM per row for a small row: the parsed fields, the rendered JSON kept beside them, and the id index. 270 000 rows of four fields load from a 14 MB snapshot in about 0.9 s and occupy 168 MB. One process owns a data file, and velo now enforces it: starting with `--data` or `--wal` takes an exclusive `flock` on `<path>.lock` and a second server pointed at the same file refuses to start rather than quietly overwriting the first one's snapshots. The lock is not blocking, so the second process says what is wrong and exits 1 instead of waiting; the kernel drops it when the owner dies, so a crashed server leaves nothing to clean up and `--watch` restarts as before, since it waits for the old process before starting the new one. Pass `--data file.json` (or set `VELO_DATA`) and velo loads that file at boot and writes it back whenever the data changed, at most once every `VELO_SAVE_MS` milliseconds (default 200):

```sh
velo run examples/api.velo :8080 --data data.json
```

A snapshot on its own means a hard kill costs whatever was written since the last one, and that window grows with the dataset because `VELO_SAVE_DUTY` widens the gap as saves get slower. `--wal file.log` closes it. Every write appends one line to a log before the response goes out, and a server started with the same log replays it over the snapshot, in order, before it accepts a connection:

```sh
velo run app.velo :8080 --data data.json --wal data.log
```

`kill -9` then loses nothing. A write of a whole row logs the row; `update`, `upsert` and `incr` log only the fields they changed, which is what keeps the log small when a route touches one field of a wide row: incrementing a counter in a 519-byte row costs 38 bytes in the log rather than 499. Replaying a change merges it, the same way the write did. A test does exactly that: writes rows, creates, updates, increments and deletes, kills the process with `SIGKILL` before any snapshot exists, starts it again and compares. Every entry holds the value a field ended up with rather than how it got there, so replay is ordered and idempotent, and a log that overlaps the snapshot it sits beside costs time and nothing else, and a half-written last line, which is what a crash mid-append leaves, is dropped while everything before it is kept. Each successful snapshot trims the log to the writes that arrived after the snapshot began, so it does not grow without end; the mark is taken before the save and the log is appended to after the row is in memory, which is what makes trimming safe.

| | writes per second |
| --- | --- |
| no log | 40 219 |
| `--wal` | 37 404 |
| `--wal` with `VELO_WAL_SYNC=1` | 1 851 |

The default is one `write` per request into the operating system's buffer, which survives velo being killed but not the machine losing power. `VELO_WAL_SYNC=1` calls `fsync` on every write, which survives that too and costs a factor of twenty; it is a disk speaking, not velo. Pick per API: a session store can lose a second of writes, a ledger cannot.

Saves are atomic (write to `.tmp`, then rename) and skipped entirely when nothing was written, so a read-only workload never touches the disk. A save takes only a read lock and skips tombstoned rows as it writes, so it neither compacts nor blocks writers: inserting at 41 000 req/s into a 200 000-row collection continues while snapshots of 16 MB are being written. The gap between snapshots adapts: velo measures how long the last save took and waits until that save has cost at most `VELO_SAVE_DUTY` percent of wall time, so a 5 MB dataset under sustained writes is not rewritten five times a second. `SIGINT` and `SIGTERM` stop the event loop and write a final snapshot before exiting, so an orderly shutdown loses nothing. Shutdown drains: velo stops accepting, answers whatever is already in flight with `Connection: close`, and exits once the last connection is gone or `VELO_DRAIN_MS` passes. A client benchmarking through a `SIGTERM` sees zero errors. without a `--wal` a hard kill can lose at most the last save interval.

## Concurrency

Every store operation is one step. It takes the collection's write lock, does the whole thing, and releases it, so no request can see a row half written and no two requests can lose one another's work inside one operation. Readers take a snapshot and render outside the lock, so a long read never blocks a write.

A route is not a transaction. A guard and the write after it are two operations, two store calls in one route are two operations, and nothing holds a lock between them. That is why anything which has to be indivisible is one operation rather than a pattern:

| instead of | write |
| --- | --- |
| `update(id, { views: find(id).views + 1 })` | `incr(id, "views")` |
| `create(body) when where("email", body.email).count() == 0` | `create(body, "email")` |
| `upsert(id, body) when where("email", body.email).count() == 0` | `upsert(id, body, "email")` |

Reads are snapshots too, so two reads in one route can disagree: `{ n: db.users.count(), rows: db.users.all() }` may report a count that does not match the length of the list if a write lands between them. Collections lock independently, so there is no atomicity across two of them, and there is no rollback: if the second of two writes in a route fails, the first still happened. Rows expire and snapshots are written on their own threads beside live traffic, each taking the same locks as anything else.

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
- **One clock read per wake.** The event loop asks the kernel the time once when `epoll_wait` returns and reuses it for the header cache, the idle timer of every connection it services, and the sweep. A static musl build does not get `clock_gettime` from the vDSO, so each one is a real syscall.
- **Remembered field positions.** Every operation that walks rows reading one field keeps the index it found it at last time and tries there first, so a wide row costs one probe instead of a walk. Rows of different shapes fall back to the search and correct the guess.
- **Borrowed sort keys.** Ordering by a text field reads the string out of the row rather than copying it, so only a value that has to be rendered, a bool or a nested object, allocates at all.
- **Partial sort.** `order` reads the `page` that follows it and stops once that many rows are in place, which is the common shape: a leaderboard wants twenty rows, not six thousand sorted ones.
- **Intersected filters.** Every `==` step before the first `page` is looked up in its own index and the posting lists are merged, so a chain costs the size of its smallest list rather than its first one.
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
| `VELO_WAL` | off | write-ahead log, same as `--wal`; every write is appended and replayed at boot |
| `VELO_WAL_SYNC` | off | `fsync` the log on every write, so a power cut loses nothing and throughput drops about twentyfold |
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
| `VELO_EXPIRE` | off | collections to sweep, as `collection.field`, comma separated, e.g. `sessions.until`; a row whose field is a Unix time in milliseconds now past is deleted |
| `VELO_EXPIRE_MS` | 60000 | how often the sweep runs |
| `VELO_COOKIE_SECURE` | off | add `Secure` to every cookie `setcookie()` writes; set it when a TLS proxy sits in front |
| `VELO_ETAG` | off | send `ETag` on 200 `GET`/`HEAD` responses and answer 304 to a matching `If-None-Match`; constant routes carry a tag computed at compile time |

## Benchmarks

`velo bench` answers the question a load generator cannot: which of *your* routes is the slow one. It compiles the file, serves it on a port of its own, drives every route it can, and prints them slowest first.

```sh
velo bench examples/api.velo -c 4 -d 1 --data snapshot.json
```

```
velo 1.43.0 benching 28 route(s), 4 conns, 1s each, 500 row(s) loaded
note: write routes run last and change the store they write to
POST /users                35450 req/s  p50   0.095 ms  p99   0.364 ms      6.6 MB/s
GET /users/top/one         38106 req/s  p50   0.096 ms  p99   0.264 ms      6.1 MB/s  38147 refused
GET /admin/users           39163 req/s  p50   0.092 ms  p99   0.268 ms   1032.2 MB/s
GET /users/1               42599 req/s  p50   0.079 ms  p99   0.259 ms      7.6 MB/s
PUT /users/1               43723 req/s  p50   0.079 ms  p99   0.261 ms      7.8 MB/s
...
GET /version               59439 req/s  p50   0.058 ms  p99   0.211 ms      9.9 MB/s
DELETE /users/:id      skipped, would delete the rows it is measuring
GET /posts/:id         skipped, no row to take a path parameter from
GET /teams/:tid/members/:mid  skipped, needs more than one path parameter
```

A route with one path parameter is given a real id from the collection its expression reads, so `GET /users/:id` runs as `GET /users/1`. A `POST`, `PUT` or `PATCH` gets a body taken from a real row of that collection, minus the `id`, so a guard like `when body.name` passes and the number describes the write rather than the 400 path; `-b '{"name":"x"}'` sets one explicitly. Write routes run after every read, because a route inserting forty thousand rows a second changes what every later measurement means, and each write route runs against the store the one before it left.

A query string is filled the same way: a route reading `query.team` gets `?team=red` when a sampled row has a `team`, so `GET /users/top/one` is measured filtering real rows rather than answering 404 to an empty filter. A body field the route names but the row does not carry, like the `body.email` a `check` insists on, is filled with a short string, so the guard passes and the write is what gets measured. `-q "team=blue&limit=5"` and `-b` set either explicitly.

Every answer of 400 or above is counted beside the rate, so a route that refuses says so instead of looking fast: `/admin/users` reads 39 163 req/s with `-H "authorization: ..."` and all-refused without it. `-H` is repeatable. Some routes cannot be driven honestly whatever is invented for them, and the refused count is how you see it: a `create` with a unique field refuses every request after the first, since the body does not change between them. `DELETE`, `HEAD` and `OPTIONS` are skipped, and so is anything whose parameter or body cannot be built; each says why. Nothing is written back to `--data`, which is loaded and never saved.

Without `--data` the store is empty and it says so, because a filter over no rows is not the number anyone wants.

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

197 tests (149 integration + 24 CLI + 8 fuzz + 16 unit): const folding, CRUD, chained reads, comparison filters, params, body fields, error codes, JSON round-trip and escaping, query params, percent-decoding, protocol edge cases, `where` filters, persistence round-trip, status overrides, paging, list-cache invalidation, graceful shutdown, a write-ahead log replayed after a SIGKILL, a second server refusing a data file that is taken, built-ins, CORS preflight, field projection and filters into nested objects, per-route metrics, indexed filters, password hashing, login and session flows, cookies read and written, cookie header injection, single-row projection, body allowlists, comparison deletes, row expiry, atomic counters that never lose an increment under eight threads, unique fields that hold through create, update and upsert when eight threads race the same email through a barrier, per-key rate limits, intersected filters, list membership through the index, grouped counts and aggregates, partial sorts, mixed-type ordering, rows of differing shapes, repeated JSON keys, cache invalidation after a bulk delete, atomic snapshots, ids that survive a reload, bare-newline requests, empty path segments, guarded routes never folding, guard reasons, per-condition validation, pipelined responses keeping their own reasons and cookies, a rate ceiling under eight threads, expiry sweeping beside live traffic, sorting, compile-error formatting, `Date` formatting, SHA-256, HMAC and PBKDF2 test vectors, cache-key construction, header hardening, sort-cache, filter-cache and chain-cache invalidation, chain cache keys that must not collide, large-list caching across writes, request headers, guards, client-supplied ids, metrics, ETag round-trip, rate limiting, raw-socket HTTP (keep-alive, pipelining, HEAD, chunked rejection, split requests, 100 concurrent connections), concurrent writes, and a read/write stress test that hammers the list, sort, filter, search, and aggregate caches from five reader threads while four writers insert, then checks the final data is consistent.

The suite has been checked by breaking the code on purpose: twenty-one faults were reintroduced one at a time across the store, persistence, HTTP parsing, the compiler and the router, and the tests were run to see which went unnoticed. Fifteen were caught. Five of the six that were not have since been covered, and finding them is what added the tests for atomic snapshots, ids surviving a reload, bare-newline requests, empty path segments, guarded routes never folding, guard reasons, per-condition validation, pipelined responses keeping their own reasons and cookies, a rate ceiling under eight threads, expiry sweeping beside live traffic, and a filtered read repeated across a bulk delete. One of the faults turned out to be a real defect rather than an introduced one: a pattern with a parameter matched a path whose segment for it was empty. Changing the row chunk size in either direction is correctly unnoticed, since it is a performance parameter and no answer depends on it.

Every test that waits for something waits until it happens or a generous deadline passes, rather than sleeping a fixed time and hoping. That is checked by running the whole suite on a machine loaded to four times its capacity, where it takes four times as long and still passes.

`tests/cli.rs` drives the built binary end to end: `check` exit codes and error text, `new` refusing to overwrite, `openapi` output parsed back as JSON, a metrics endpoint, `include` across a directory of files, a directory named as a whole, a file appearing in a watched directory, serving on a Unix socket, a program using every documented store operation and built-in, `--watch` restarting on a change to a route file or a folded-in asset and surviving a broken save, and a `POST` surviving a `SIGTERM` restart through the snapshot file.

`tests/fuzz.rs` adds eight deterministic robustness tests. One of them mixes writes into the reads: it holds 1 200 rows, applies six hundred inserts, updates, upserts, deletes and bulk deletes chosen at random, keeps the collection above the size where the index switches on, and after every single write checks a filtered count and the total against the same figures tracked in the test. Another builds 1 200 rows of randomly shaped JSON, every field optional and in a random order, values drawn from numbers, text, `null`, `true` and the empty string, then checks every read against the same answer counted by hand in the test: equality and its negation must cover every row, two filters must agree whichever order they are written in, a sum must ignore the values that only look numeric, a list must hold exactly the ids the filter should have found, and every top-`n` must equal the full sort truncated. The same rows are loaded a second time in a shuffled order and must answer identically, which is what catches a read that depends on where the previous row happened to keep its fields. The rest are: 2 000 mutated sources and 2 000 random byte strings through the compiler, 300 connections of malformed and truncated HTTP, 400 connections carrying byte-level mutations of otherwise valid requests (every answer must still be a well-formed status line), and oversized header and body requests. They also cover slow drip-feeding clients. Every test binary starts a watchdog on its first helper call: if the suite is still running after `VELO_TEST_TIMEOUT` seconds, 300 by default against a suite that takes nine, it writes what happened straight to the process's stderr, past the harness's output capture, and exits. A test that waits forever is worse than one that fails, because it says nothing and a run that never ends is a run nobody reads. This project has produced three of them, each found by accident while trying to break something else: an assertion inside a barrier loop, a wait on a server process that had been asked to refuse and did not, and a lock taken without `LOCK_NB`. The suite is checked by breaking velo on purpose, and `./mutants.sh` does it repeatably: `mutants/` holds one patch per fault, each a mistake a person could plausibly make, and the script applies each in turn, runs the suite, and reports whether it was caught. A patch that survives is a hole. Fifty-four faults are recorded, across the HTTP parser, the event loop, the router, the compiler, the store, the value tree, the write-ahead log and the data-file lock. Fifty-two are caught, and the run that says so was a single pass over all of them rather than the batches they were written in, which is a different claim: patches can collide, and a run can leave state behind. Two change no behaviour at all and are listed as such rather than counted as wins: a weaker hash the map still compares keys behind, and a parameter count only ever read within range. The list of real faults with no test is empty, and a fault added to it would be a hole someone had decided to live with.

The last one to be closed was the 256 kB cap on pending pipelined responses. It needs a client that floods a connection and never reads, and what it protects is memory rather than an answer, so the test watches the server's own `VmRSS`: three thousand pipelined requests for a 60 kB page, 180 MB of answers nobody is reading, and the process must not grow by a quarter of it. Without the cap it grows by nearly all of it.

```sh
./mutants.sh              # every fault, about half an hour
./mutants.sh no-drain     # one of them
```

It builds with a `mutants` profile that keeps `opt-level = 3` but drops link-time optimisation, since a fault is being looked for rather than a benchmark run: a rebuild-and-test cycle costs 30 seconds rather than 70, and the race-sensitive faults are still caught, which was worth checking before trusting it.

The rounds that produced them: seventeen against the HTTP parser and the event loop, of which ten were caught first time; twelve against the router and the compiler, of which ten were caught and two change nothing; and twenty-five against the store, the value tree, the log and the lock, every one of them caught, which is what a file that has been mutation-tested at every release looks like. The seventeen were: accepting a chunked body, accepting an oversized one, accepting conflicting `Content-Length` headers, keeping a connection alive after `Connection: close`, keeping an HTTP/1.0 one alive by default, sending a body with a `HEAD`, ignoring `Expect: 100-continue`, ignoring `If-None-Match`, framing a `304` as though it had a body, letting a rate window never reset, never refusing a connection past `VELO_MAX_CONNS`, never sweeping an idle connection, never timing out a drip-fed header, exiting instead of draining, removing the pending-write cap, ignoring `VELO_REAL_IP_HEADER`, and a rate limit off by one. Ten were caught. The seven that were not are covered now, and each of the new tests fails against the fault it was written for. Three of them were tests that already existed and did not test what their name said: the drain test wrote its request after `shutdown()` and the server usually answered it before noticing, the slow-client test could not tell a closed connection from its own read timing out, and the `304` test never looked at how the response was framed. The one fault still uncovered is the removal of the 256 kB cap on pending pipelined responses, which needs a client that pipelines faster than it reads, and it is written down here rather than left unsaid. The router and the compiler answered better: an empty path segment matching a parameter, a parameter beating a static segment, a trailing slash left significant, the method ignored, duplicate routes accepted, a `POST` losing its 201, and four ways of const-folding a route that must not be folded, including a guarded one, were all caught.

`VELO_FUZZ_ROUNDS` raises the iteration counts for a longer hunt; 40 000 compiler mutations and 4 000 mutated requests have been run clean. They assert the process never panics and that the server still answers a normal request afterwards.

## CI

Every ```velo``` block in this file is compiled by the test suite, so an example that stops matching the language fails a build rather than sitting here misleading someone.

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
| `src/bench.rs` | the load generator, shared by `velobench` and `velo bench` |
| `src/bin/velobench.rs` | load generator CLI |
| `src/bin/velomicro.rs` | in-process dispatch microbenchmark, `velomicro [rows]` |
| `src/wal.rs` | write-ahead log: append, replay, trim, and the data-file lock |
| `CHANGELOG.md` | every release, newest first |
| `check.sh` | the gate: format, clippy, tests, examples, microbenchmark, live throughput |
| `mutants.sh`, `mutants/` | 54 faults introduced on purpose, one patch each, to check what the suite catches |
| `bench/baseline.json` | the per-operation times `velomicro --check` compares against |

## Build notes

`.cargo/config.toml` targets `x86_64-unknown-linux-musl` with `rust-lld`, so the build needs no system C toolchain. Incremental compilation is off there: it bought little on a project this size and its cache grew to gigabytes across many rebuilds. Remove that file to build against glibc with `cc`.

The write path is bounded by the allocator rather than by anything velo does. An insert costs about 2 microseconds in process, of which roughly six allocations account for a third: on this musl build a single `Box::new(u64)` measures 50 nanoseconds and a `Vec::with_capacity(64)` 67, where glibc would be nearer 15 to 20. Removing one allocation from that path therefore buys about 1 per cent, which is why the reads got the attention instead. Building against glibc, by removing `.cargo/config.toml`, should make every write cheaper without changing a line.

Requirements: Linux 4.5 or newer (the workers share the listener with `EPOLLEXCLUSIVE`), Rust 1.75 or newer, no crates.

## Releases

The five most recent, with every release in [CHANGELOG.md](CHANGELOG.md).

**v1.50.0** — the whole mutation set run in one pass for the first time, and the last hole closed. Every patch had been checked in the batch it was written in, which is not the same claim as the set holding together: patches can collide and a run can leave state behind. Fifty-four faults, one pass, 51 caught, 0 survived, 0 broken, and a clean tree at the end, matching the batches exactly. That left one fault named as open, the 256 kB cap on pending pipelined responses, which has a test now and makes it 52. What the cap protects is memory rather than an answer, so the test watches the server's own `VmRSS`: a client floods one connection with three thousand pipelined requests for a 60 kB page, 180 MB of answers it never reads, and the process must not grow by a quarter of that. Removing the cap fails it. The list of faults with no test is empty now, and `mutants.sh` keeps the list rather than deleting it, so that adding to it is a decision someone has to write down.

**v1.49.0** — the suite runs in a third of the time, which matters most for the tool that runs it fifty-four times. `./mutants.sh` took an hour, and a quality tool nobody waits for is a quality tool nobody runs. Two things were in the way. The `velo bench` test drove nine routes for a second each, three times over, and was 23 of the CLI suite's 34 seconds on its own; `-d` now takes a fraction of a second, which the command wanted anyway for a quick pass over a large file, and the test asks for 0.2. That alone took the CLI binary from 33.7 seconds to 5.0. Then a `mutants` build profile that keeps `opt-level = 3` but drops link-time optimisation, since these builds exist to look for a fault rather than to measure anything: a rebuild-and-test cycle is 30 seconds rather than 70. The race-sensitive faults, an increment reading outside the lock and a uniqueness check doing the same, are still caught under it, which was worth checking before trusting the number.

**v1.48.0** — the store's faults are recorded too, so the whole suite is guarded against going backwards. Twenty-five more patches, one for each fault this project actually shipped a fix for: an increment that reads outside the write lock, a uniqueness check that does the same, an index blind to a duplicate, an upsert split across two locks, a list filter that keeps only its first value or forgets to trim, a cache key that lets `in` and `==` collide, a grouped count that leaves its keys unsorted or keeps a null as a group, two more group cache keys that drop what makes them unique, an index that cannot tell a missing field from an empty one, a merge that stops at the top level or at one level down, a path lookup with no fallback or only one level, a chained projection blind to nesting, a leaf that overrides its parent, a missing path that becomes a null, four ways the write-ahead log can lose or mangle a write, and two ways the data-file lock can fail to lock. All twenty-five are caught. That is what a file mutation-tested at every release looks like, and it is worth contrasting with the seven that survived the first round against the event loop. `mutants.sh` now separates a fault that changes no behaviour from one that is real and untested, and prints the second as open rather than folding it into a pass: fifty-four faults, fifty-one caught, two equivalent, one open.

**v1.47.0** — the router and the compiler got the same treatment, and the method itself became repeatable. Twelve faults: an empty path segment matching a parameter, a parameter beating a static segment, a trailing slash left significant, the method ignored so any route answers any verb, duplicate routes silently accepted, a `POST` losing its 201, and four ways of const-folding a route that must never be folded, a guarded one among them. Ten were caught. The two that survived change no behaviour: a weaker hash that the map still compares keys behind, and a parameter count that is only ever read within range. Saying that plainly is better than writing a test that cannot fail. What is new beyond the finding is `./mutants.sh`, which turns the thing I have been doing by hand all along into something anyone can run: `mutants/` holds one patch per fault, twenty-nine of them now, and the script applies each, runs the suite, reports caught or survived, and puts the tree back even if it is interrupted. Three faults are listed in it as equivalent, so a reader can see what is claimed and what is not.

**v1.46.0** — a mutation round against the HTTP parser and the event loop, the two files that had never had one. Seventeen faults were introduced on purpose and ten were caught: chunked bodies accepted, oversized ones accepted, conflicting `Content-Length` headers accepted, `Connection: close` ignored, HTTP/1.0 kept alive, a `HEAD` given a body, `Expect: 100-continue` ignored, `If-None-Match` ignored, a rate limit off by one, and `VELO_REAL_IP_HEADER` ignored. Seven survived, and three of those were tests that already existed and did not test what their name claimed: the drain test wrote its request after `shutdown()` and the server usually answered it before it noticed, so it passed whether or not draining worked; the slow-client test could not tell a connection the server had closed from its own read timing out; and the `304` test checked the status line but never how the response was framed, so a `304` announcing a `Content-Length` it would never send went unnoticed. All three now fail against the fault they were written for. Four new gaps are covered too: a rate window that never resets and locks a client out forever, a server that never refuses past `VELO_MAX_CONNS`, an idle connection that is never swept, and a drip-fed header that never times out. The one fault still uncovered is the removal of the pending-write cap, which needs a client that pipelines faster than it reads; it is written down rather than left unsaid. The mutation script itself had a bug worth naming: it wrote to `src/serve` instead of `src/serve.rs`, so the first run measured an unmutated build and would have reported everything as caught. It asserts the mutation landed now.
