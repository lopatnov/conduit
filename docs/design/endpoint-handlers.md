# Design: WASM and Rhai endpoint handlers

> **Status: proposal, nothing is implemented.** This document is research and a design with options; every choice marked
> **Decision** is the owner's to make. Related issues: #290 and #291 (serverless-style function routing to Node.js and
> Python workers), #187 (QuickJS middleware). None of them asks for exactly this, so this document is not tied to one.
> Prior art was read from source where possible (Spin, Wasmtime 50.0.1, `wasi:http`, `wasi:keyvalue`, Fastly Viceroy, `wstd`);
> statements about Cloudflare Workers and wasmCloud come from their public documentation and are marked as such.

---

## Contents

- [The question](#the-question)
- [Short answer](#short-answer)
- [What Conduit has today](#what-conduit-has-today)
- [How others do it](#how-others-do-it)
- [Design](#design)
  - [Where a handler sits in the request pipeline](#where-a-handler-sits-in-the-request-pipeline)
  - [Routing and route parameters](#routing-and-route-parameters)
  - [The WASM interface: three options](#the-wasm-interface-three-options)
  - [The Rhai interface](#the-rhai-interface)
  - [Host capabilities](#host-capabilities)
  - [Security model](#security-model)
  - [Limits and timeouts](#limits-and-timeouts)
  - [Configuration](#configuration)
  - [Crates, Cargo features and the `webapp` bundle](#crates-cargo-features-and-the-webapp-bundle)
  - [Rhai and WASM parity](#rhai-and-wasm-parity)
- [Phased plan](#phased-plan)
- [Decisions for the owner](#decisions-for-the-owner)
- [What is not known yet](#what-is-not-known-yet)

---

## The question

Rhai scripts and WASM plugins are *middleware* today: they run before the request goes to an upstream, can stop it
with their own answer, and otherwise pass it on. The owner asks whether a script or plugin could instead be the
**endpoint**: Conduit routes the request, the handler produces the response, and no upstream is involved. Three
sub-questions:

1. Can the routing result (which route matched, path parameters like `{id}`) be passed to the handler?
2. What can such a handler do on its own, and which host APIs (files, a database, outbound HTTP) does it need?
3. How would the configuration look, and could this become a build such as `conduit-webapp`?

## Short answer

1. **Yes.** Conduit already matches the route; it can hand the handler the method, the full URL, the headers, the
   body, the matched route pattern, named path parameters and the "rest of the path" for a wildcard. A handler mounted
   on a wildcard (`/app/{*rest}`) can also do its own routing inside, so a whole small web framework compiled to WASM
   works.
2. **Without host APIs** a handler can only compute from the request and its `config`: JSON answers, redirects,
   templated text, validation, mock APIs. For an application it needs **capabilities** that the host grants one by one,
   denied by default: read-only files, a key-value store, SQLite, outbound HTTP to an allowlist, named secrets. This is
   exactly the model of Spin, Fastly Compute, wasmCloud and Cloudflare Workers.
3. A new `handler:` action on a `routes[]` entry (next to `proxy:` and `static:`), named resources declared once and
   granted per handler, and a `webapp` Cargo feature bundle. See [Configuration](#configuration).

---

## What Conduit has today

| | Rhai middleware | WASM middleware | `ext_proc` |
| --- | --- | --- | --- |
| Crate | `conduit-script-rhai` | `conduit-plugin-wasm` | `conduit-ext-proc` |
| Engine | Rhai 1.27, `sync` feature, AST cached by path | Wasmtime 50, Cranelift, **core modules only** (no component model, no WASI) | gRPC client to an external processor |
| Invocation | synchronous, on the request thread | synchronous, on the request thread, new `Store` + `Linker` per call | async stream |
| Sees | method, path, query, headers | method, path, query, URI, headers, client IP, request ID | headers (and body by mode) |
| Request body | no | no | yes, by mode |
| Can answer | status, body, headers | status, body, headers, redirect | yes |
| CPU limit | 500,000 operations | 10,000,000 fuel per call | the processor's own |
| Memory limit | strings 1 MiB, arrays/maps 65,536 | 16 MiB linear memory | n/a |
| I/O | none | none | n/a |
| State | none | none (new instance per call) | external |
| Failure | fail-open | fail-open | configurable |

Two facts shape the design:

- **There is no named-parameter routing.** `routes[].match.path` is a glob (`*` one segment, `**` any sub-path,
  `?` one character) in `crates/conduit-proxy-http/src/routes.rs::glob_match`. Nothing captures a segment by name.
- **Local handlers already exist**: `LocalHandler` (`crates/conduit-runtime/src/proxy/ctx.rs`) and the
  `LocalHandlerImpl` trait (`crates/conduit-core/src/handler/mod.rs`) serve static files, health, metrics, ACME and
  hot-reload without an upstream, after the guard chain. A script/plugin endpoint is one more local handler; the
  static handler shows how CORS and security headers (`extra_headers`) reach a locally produced response.

Middleware is fail-open by design (a broken guard must not take a site down when real access checks live in config).
**An endpoint cannot be fail-open**: there is nothing to "pass through" to. A handler that traps, times out or cannot
be loaded must answer with an error status. This is the largest semantic difference from today's scripting.

---

## How others do it

**Fermyon Spin** (read from `spinframework/spin`, `crates/manifest`, `crates/routes`, `crates/trigger-http`,
`crates/factor-*`).

- A component is a WASI HTTP component (`wasi:http/incoming-handler@0.2.x`, and `wasi:http/handler@0.3.x`; Spin's
  `wit/deps` carries `http@0.3.2`).
- Routes: exact (`/users`), named segment (`/users/:id`), trailing wildcard (`/static/...`). The router is
  `routefinder`.
- Route data reaches the guest **as request headers**, computed by the host in `trigger-http/src/headers.rs`:
  `spin-full-url`, `spin-path-info` (the wildcard tail), `spin-matched-route`, `spin-raw-component-route`,
  `spin-base-path`, `spin-client-addr`, and one `spin-path-match-<name>` per named segment.
- Capabilities are **per component, deny by default**, in the manifest: `allowed_outbound_hosts`,
  `key_value_stores = ["default", ...]`, `sqlite_databases = [...]`, `files` (mounted read-only), `variables`. A store
  is a **label**; which backend a label means (in-memory, a file, Redis, Azure Cosmos …) is set in a separate
  runtime config file by the operator. The component never names a backend.
- Host buffers are capped (`MAX_HOST_BUFFERED_BYTES`); instance reuse is configurable
  (`max_instance_reuse_count`, default 1 for WASIp2, 128 for WASIp3; `request_timeout`; `idle_instance_timeout`).

**Wasmtime 50.0.1** (the version Conduit pins) ships the pieces a host needs:

- `wasmtime-wasi-http` with features `p2` and `p3`; `handler.rs` has `ProxyPre`/`Proxy` enums that run either a
  0.2 `incoming-handler` or a 0.3 `handler` component. `WasiHttpHooks` lets the host filter forbidden headers and
  decide outgoing requests (`send_request`) — the hook for an outbound allowlist.
- `wasmtime-wasi`: `preopened_dir` with directory and file permissions (read-only mounts), `socket_addr_check`,
  `allow_ip_name_lookup`.
- `wasmtime-wasi-keyvalue`: the `wasi:keyvalue` host with an in-memory store only; a real backend is the host's job.
- `wasmtime-wasi-config`: `wasi:config` (key/value configuration for the guest).
- All of it needs `wasmtime/component-model` and `wasmtime/async`; `p3` also needs `component-model-async`.

**`wasi:keyvalue`** (`WebAssembly/wasi-keyvalue`, `wit/store.wit`): `open(identifier) -> bucket`, and `get`, `set`,
`delete`, `exists`, `list-keys(cursor)` on a bucket; `atomics` (increment, compare-and-swap) and `batch` interfaces.
There is **no standard SQL interface** that hosts implement today; Spin has its own `spin:sqlite` interface.

**Fastly Compute** (read from `fastly/Viceroy`, `wasm_abi/wit/deps/fastly/compute.wit`): its own WIT with
`kv-store`, `config-store`, `secret-store`, `backend`, `cache`, `geo`. Outbound HTTP goes only to **backends declared
in the service configuration**; a guest can register a dynamic backend only where that is enabled. A new instance per
request.

**Cloudflare Workers** (public documentation): a JavaScript/WASM `fetch(request, env, ctx)` handler; resources are
**bindings** declared in the Wrangler config (KV namespaces, D1 SQL databases, R2 buckets, service bindings, `vars`,
secrets) and appear on `env`. Outbound `fetch` reaches the public internet without an allowlist. CPU time per request
is limited.

**wasmCloud** (public documentation): components import standard interfaces (`wasi:http`, `wasi:keyvalue`,
`wasi:blobstore`, `wasi:config`); the application manifest **links** each import to a capability provider (Redis,
NATS KV, a vault …). Same idea as Spin's labels, with providers running as separate processes.

**What to copy**

- The handler is a standard WASI HTTP component, so the developer uses ordinary tooling and the module also runs on
  Spin or wasmCloud.
- Capabilities granted per handler, denied by default, by **label**; the operator binds a label to a backend.
- Outbound HTTP through a host hook with an allowlist (Spin, Fastly), not open like Workers.
- Route parameters as host-computed values the guest cannot forge.

**What not to copy**

- Spin's `:id` route syntax: Conduit's Admin API is Axum 0.8, whose router uses `{id}` and `{*rest}`; one syntax in one
  product is better (see [Routing](#routing-and-route-parameters)).
- A separate runtime-config file: Conduit has one config file and hot reload; resources go in it.
- Out-of-process capability providers (wasmCloud): too much machinery for a single-binary proxy.

---

## Design

### Where a handler sits in the request pipeline

```text
request_filter()
  FilterChain: XRequestId → IpGuard → CORS preflight → HealthBypass → Limits → RateLimit → Consumers
               → BasicAuth → ApiKey → JWT → ForwardAuth → Redirect → FaultInjection → Middleware (Rhai/WASM)
  route matched: routes[i] has `handler:`  →  LocalHandler::Endpoint { handler, params }
  build_handler() → EndpointHandler::handle(session)
       read body (bounded) → run script / component (bounded) → write response (+ CORS/security headers)
logging(): access log, metrics (per handler: invocations, errors, duration, fuel)
```

- Every guard runs before the handler, as for `proxy` and `static`: IP filter, rate limits, authentication and
  middleware protect an endpoint without new code. JWT claims (`RequestCtx.jwt_claims`) and the consumer name can be
  passed to the handler (below).
- The handler is a `LocalHandlerImpl`, so the proxy-only stages (`upstream_request_filter`, `ResponseFilterChain`,
  retries, caching, mirroring) do not run. CORS and security headers are added the way the static handler adds them.
  `responseTransform` and `X-Response-Time` would need a call in the handler path; whether to apply them is listed under
  [What is not known yet](#what-is-not-known-yet).
- Responses of handlers are **not cached** in the first phases (decision #9 caches proxy responses only).

### Routing and route parameters

**Path patterns.** Extend `match.path` with two forms, in Axum 0.8 / `matchit` syntax:

| Pattern | Matches | Captures |
| --- | --- | --- |
| `/users/{id}` | `/users/42` | `id = "42"` (one segment, percent-decoded) |
| `/files/{*rest}` | `/files/a/b.txt` | `rest = "a/b.txt"` |
| `/users/*`, `/api/**` | as today | nothing |

This changes no existing config: RFC 3986 §3.3 does not allow an unencoded `{` or `}` in a path, so a pattern with
braces never matched a valid request before. Captures are made only for routes whose action is `handler:` (a `proxy`
route does not need them; using them in `rewrite` is a possible later extension). Matching order stays "first route
in declaration order wins".

**What the handler receives** (both engines):

| Value | Example |
| --- | --- |
| method, full URL, headers, body | `GET https://app.example.com/api/users/42?x=1` |
| `route` (the pattern that matched) | `/api/users/{id}` |
| `params` | `{ "id": "42" }` |
| `rest` (the `{*name}` capture, if any) | `a/b.txt` |
| `clientIp`, `requestId` | `203.0.113.7`, the `X-Request-ID` value |
| `consumer`, `jwt` claims | only when the route has `consumers`/`jwtAuth` **and** the handler's `allow.identity: true` |

**How the WASM guest gets them.** `wasi:http` has no slot for route data. Two ways:

- *Headers*, Spin-style: the host **removes** every incoming header with the reserved prefix `conduit-` and then sets
  `conduit-route`, `conduit-param-<name>`, `conduit-path-rest`, `conduit-client-ip`, `conduit-request-id`. Works with
  any `wasi:http` guest and any framework, no extra WIT. The removal is mandatory: without it a client could forge a
  parameter or a client IP.
- *An import interface* `conduit:handler/request-context` (`route()`, `params()`, `client-ip()`, …). Typed and
  unforgeable, but every language needs bindings for it.

**Recommendation:** headers in the first phase (they cost nothing to the guest author), the WIT interface when the
`conduit:*` interfaces are added for SQLite anyway.

**Routing inside the handler.** A handler on `/app/{*rest}` receives everything under `/app/` and may route by itself.
This works today in Rust: the Bytecode Alliance's `wstd-axum` crate runs an ordinary `axum::Router` inside a
`wasi:http` component (read from `bytecodealliance/wstd`, `axum/src/lib.rs`). Conduit's routing then only chooses
*which* handler, and does the TLS, limits and authentication in front of it.

### The WASM interface: three options

**Option A: extend today's core-module ABI.** Add an `on_handle() -> i32` export, body host functions
(`conduit_get_body`, chunked reads) and capability host functions (`conduit_kv_get`, `conduit_fetch`, …), all in the
existing `(ptr, len)` style.

- For: no new Wasmtime features (no binary growth from the component model); one ABI for middleware and handlers;
  small modules.
- Against: a non-standard ABI that every language needs a hand-written SDK for; no async in the guest, so an outbound
  request or a database query blocks a request thread (Conduit runs WASM synchronously today); every capability is
  new ABI surface to design, document and keep stable; modules are not portable.

**Option B: WASI HTTP components (`wasi:http/proxy` 0.2 and `wasi:http/service` 0.3).** The handler is a component
that exports `incoming-handler` (0.2) or `handler` (0.3). Conduit runs it with `wasmtime-wasi-http`.

- For: standard tooling in many languages (Rust with `wstd` or `wit-bindgen`, `componentize-py`, `jco`/ComponentizeJS,
  TinyGo, C#), modules that also run on Spin, wasmCloud and other hosts; async I/O in the guest, so an outbound request
  or a query does not hold a thread; outbound HTTP, files, configuration and key-value already have standard
  interfaces and host implementations in Wasmtime; request and response bodies are streams, not buffers.
- Against: the `component-model` and `async` Wasmtime features and `wasmtime-wasi`/`wasmtime-wasi-http` add binary
  size (unknown until measured) and dependencies (the `lawyer` check applies); SQL has no standard interface, so SQLite
  is a Conduit-specific WIT; WASI 0.3 changed the interface shape from 0.2, and supporting both costs code.

**Option C: B for handlers, A stays for middleware.** Middleware keeps its small synchronous ABI (it runs on every
request and must stay cheap); handlers use components. Two WASM paths in the codebase, but each fits its job.

**Recommendation: C.** Handlers are application code, where standard tooling and async I/O matter more than the last
bit of size; middleware is per-request plumbing, where the current ABI's cost profile is right. Accept 0.2
(`incoming-handler`) first, since every toolchain emits it; add 0.3 (`handler`, native `async func`) through
`ProxyPre::P3` once a toolchain the owner wants to support emits it. The extra size goes to the opt-in `webapp`
bundle only, not to `default` or `standard`.

A Rust handler under option B (illustrative; `wstd` is the Bytecode Alliance's async std for WASI):

<!-- docs-check: skip (Rust, not a config) -->
```rust
use wstd::http::{Body, Request, Response, StatusCode};

#[wstd::http_server]
async fn main(req: Request<Body>) -> Result<Response<Body>, wstd::http::Error> {
    // set by Conduit from the route `/api/users/{id}`; a client cannot forge it
    let id = req.headers().get("conduit-param-id").and_then(|v| v.to_str().ok()).unwrap_or("");
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"id":"{id}"}}"#)))?)
}
```

### The Rhai interface

A handler script defines `fn handle(req)` and returns a response map:

<!-- docs-check: skip (Rhai, not a config) -->
```rhai
fn handle(req) {
    let id = req.params.id;                       // from /api/users/{id}
    let user = kv::get("users", "user:" + id);    // needs allow.kv: [users]
    if user == () {
        return #{ status: 404, body: "not found" };
    }
    #{ status: 200, headers: #{ "content-type": "application/json" }, body: user }
}
```

- `req`: `method`, `url`, `path`, `query`, `headers` (a map, lower-case names), `params`, `rest`, `route`, `body`
  (a string when UTF-8, otherwise a `Blob`), `client_ip`, `request_id`, and `consumer`/`jwt` when granted.
- Return: `#{ status, headers, body }`; a bare string is `200 text/plain`; `()` is `204`.
- Capabilities are Rhai modules registered only when granted: `kv::get/set/delete/exists/keys/increment`,
  `sql::query(db, sql, params)` and `sql::execute(...)`, `http::fetch(url, opts)`, `files::read(path)`,
  `secrets::get(name)`, `json::parse/stringify`, `log::info/warn/...`. An ungranted module does not exist, so calling
  it is a compile-time error the same way a typo is (`conduit validate` reports it).
- Rhai is synchronous. A handler that uses no I/O capability runs inline like middleware does; one that is granted
  any I/O capability runs on a **bounded blocking pool** (`tokio::task::spawn_blocking` under a semaphore), so a slow
  query does not hold a Pingora worker thread. The wall-clock deadline is enforced with `Engine::on_progress`.
- `import` of other `.rhai` files from the handler's own directory only (a `FileModuleResolver` rooted there), so a
  handler can be split into modules.

### Host capabilities

All capabilities are **denied unless the handler's `allow:` grants them**, and each grant names a resource declared by
the operator (a label, as in Spin), never a raw path or URL chosen by the handler.

| Capability | WASM (option B) | Rhai | Backends | Phase |
| --- | --- | --- | --- | --- |
| Configuration | `wasi:config` (the handler's `config:`) | `config` variable | config file | 1 |
| Log | stdout/stderr → Conduit log, rate-limited | `log::*` | tracing | 1 |
| Clock, random | `wasi:clocks`, `wasi:random` | `time::now()`, `random::*` | host | 1 |
| Secrets | `wasi:config` keys from `secrets` | `secrets::get(name)` | environment variable, file | 2 |
| Files (read) | `wasi:filesystem` preopen, read-only | `files::read(path)` | a directory, opened beneath its root (reuse `conduit-static`'s `open_beneath`) | 2 |
| Key-value | `wasi:keyvalue` (store, atomics, batch) | `kv::*` | `memory` (DashMap, per process), `redb` (file), `redis` (shared between nodes) | 2 |
| Outbound HTTP | `wasi:http/outgoing-handler` via `WasiHttpHooks::send_request` | `http::fetch` | Conduit's HTTP client; or a named Conduit upstream, using its load balancing and health | 3 |
| SQLite | `conduit:sqlite` WIT (shape close to Spin's `spin:sqlite`) | `sql::*` | `rusqlite` | 3 |
| Postgres | `conduit:postgres` WIT or behind `conduit:sql` | `sql::*` | `tokio-postgres` + pool | 4, on demand |
| Files (write) | read-write preopen | `files::write` | a directory | not planned without a use case |

Notes:

- **Outbound to a Conduit upstream.** `fetch: [upstream:users-api]` lets a handler call a group Conduit already
  balances and health-checks, so a "backend for frontend" handler gets retries and circuit breaking for free. This is
  the closest answer to "pass the routing on": the handler can delegate to the normal proxy machinery.
- **KV backends.** `memory` is the default and is lost on restart; `redb` is a pure-Rust embedded store with ACID
  transactions (no C code); `redis` reuses the client Conduit already has behind the `redis` feature (decision #14's
  rate limiter). `sled` is not proposed: it has had no stable release for years.
- **SQLite** needs C code (`rusqlite` with the bundled SQLite) and a size budget; it is the most requested thing for a
  "website" build, so it is phase 3 rather than never.
- **Transactions across capabilities** (KV and SQL in one transaction) are not offered.

### Security model

- **Deny by default.** No `allow:` means no I/O at all: today's sandbox. Each grant is named in the handler's config,
  and `conduit validate` lists them (`conduit validate --explain` could print a table of handler → capabilities).
- **Labels, not paths.** A handler asks for `kv: [sessions]`; only the operator decides that `sessions` is
  `./data/sessions.redb`. A handler cannot open another store by guessing its name; `wasi:keyvalue`'s `open(id)`
  fails with `access-denied` for an ungranted label.
- **Outbound HTTP:** an allowlist of origins (`scheme://host[:port]`), matched after normalisation; redirects are not
  followed unless the target is also allowed; after DNS resolution, loopback, link-local, private (RFC 1918, RFC 4193)
  and the Admin API address are refused unless the grant names them explicitly (SSRF and DNS rebinding); the
  `forbidden_header_filter` hook drops hop-by-hop headers and `Host` rewriting tricks. Per-invocation caps on count,
  time and response size.
- **Files:** read-only preopens of directories the operator lists; paths opened beneath the root (no `..`, no symlink
  escape: the `conduit-static` `open_beneath` code already does this, including the Windows cases fixed in #595/#619).
- **Secrets:** only names listed in `secrets` are readable, each mapped to an environment variable or a file; the
  process environment is never exposed wholesale, and secret values are not logged.
- **Reserved headers:** incoming `conduit-*` headers are removed before the host sets its own.
- **Failure is closed.** A handler that cannot load, traps, exceeds a limit or returns an invalid response gets
  `500` (or `503` when the cause is the concurrency limit, `504` on the deadline) with a generic body; details go to
  the log, never to the client. The last module or script that loaded stays in use after a reload whose new file is
  broken, exactly as middleware does today.
- **Resource exhaustion:** every limit below has a default, and a handler that hits `maxConcurrent` is answered with
  `503` rather than queued without bound.
- A handler is application code: it can still have bugs (SQL injection through `sql::execute` with a concatenated
  string, for example). The SQL API takes parameters separately and the docs show only the parameterised form.

### Limits and timeouts

Every value is configurable per handler; site and global defaults may be added later. Defaults are proposals to be
measured in phase 1.

| Field | Default | Meaning |
| --- | --- | --- |
| `timeoutMs` | 10000 | Wall-clock deadline for the whole invocation, including I/O. WASM: epoch interruption (one ticker thread) with `epoch_deadline_async_yield_and_update`; Rhai: `on_progress`. → `504`. |
| `fuel` | 100000000 | WASM CPU budget per invocation (10× middleware's: a handler does real work). |
| `maxOperations` | 5000000 | Rhai CPU budget (10× middleware's). |
| `memoryMiB` | 64 | WASM linear memory (and tables) per instance, through a `ResourceLimiter`. |
| `maxRequestBodyBytes` | 1048576 | Larger bodies are refused with `413` before the handler runs; `0` means the handler gets no body. |
| `maxResponseBodyBytes` | 10485760 | A handler that writes more is cut off and the response aborted. |
| `maxConcurrent` | 256 | Invocations of this handler at once; over it → `503`. |
| `maxOutboundRequests` | 16 | Outbound requests per invocation. |
| `outboundTimeoutMs` | 5000 | Per outbound request. |
| `maxKvValueBytes` | 1048576 | Per value written. |
| `maxSqlRows` | 10000 | Rows returned per query. |

Instance reuse: phase 1 creates a new instance per request, as middleware does and as Spin does for WASIp2 (an
instance-pre is cached, so instantiation is cheap). Reuse (`maxInstanceReuse`, as in Spin for WASIp3) is a later
optimisation, and only for 0.3 components, because a reused instance keeps the previous request's memory.

### Configuration

The proposal adds a `handler:` action to a `routes[]` entry, and a `resources:` block declared once in `global`.

<!-- docs-check: skip (design proposal: handler and resources keys do not exist yet) -->
```yaml
global:
  resources:
    kv:
      sessions: { backend: redb, path: ./data/sessions.redb }
      cache:    { backend: memory, maxEntries: 100000 }
      shared:   { backend: redis, url: "redis://127.0.0.1:6379/2" }
    sqlite:
      app: { path: ./data/app.db }
    files:
      templates: { path: ./templates }
    secrets:
      stripe_key: { env: STRIPE_KEY }
      db_password: { file: /run/secrets/db_password }

sites:
  - host: app.example.com
    port: 8080
    routes:
      # A WASM component that answers /api/users/{id}
      - match: { path: "/api/users/{id}", method: [GET, PUT, DELETE] }
        handler:
          wasm: ./handlers/users.wasm
          config: { pageSize: 50 }
          allow:
            kv: [sessions]
            sqlite: [app]
            fetch: ["https://api.stripe.com", "upstream:billing"]
            files: [templates]
            secrets: [stripe_key]
            identity: true            # pass the JWT claims / consumer name
          limits: { timeoutMs: 5000, memoryMiB: 32, maxConcurrent: 64 }

      # A Rhai script, no capabilities: computes from the request only
      - match: { path: "/hello/{name}", method: [GET] }
        handler:
          rhai: ./handlers/hello.rhai

      # A whole app routed inside one component
      - match: { path: "/app/{*rest}" }
        handler:
          wasm: ./handlers/app.wasm
          allow: { kv: [cache], files: [templates] }

      # Everything else: the static site
      - match: { path: "/**" }
        static: ./public
```

Rules for validation:

- `handler` excludes `proxy` and `static` on the same entry; exactly one of `wasm` / `rhai`.
- Every label in `allow` must exist in `global.resources`; an unknown label is an error, not a warning (a missing
  grant is not a typo to ignore).
- `{name}` captures must be unique in a pattern, and `{*name}` only last.
- `handler.wasm` must be a component when option B is chosen; a core module is reported with a message that points to
  `type: wasm` middleware instead.
- Without the feature compiled in, a `handler` route is a **startup error**, not a warning: unlike middleware, there
  is nothing to fall back to, and serving `404` for an application silently would be worse.
- **Hot or cold.** Handlers, `allow` and `limits` reload hot (ArcSwap, as everything else). Resource definitions
  reload hot when the backend can be reopened safely (memory, Redis); changing the `path` of a `redb` or SQLite
  resource needs a restart (decision #12's cold list gains `global.resources.{kv,sqlite}.*.path`).

The JSON schema (`schema/conduit.schema.json`), `docs/configuration.md` and a new `docs/handlers.md` guide would
follow the implementation, not this design.

### Crates, Cargo features and the `webapp` bundle

| Crate | Content |
| --- | --- |
| `conduit-handler` | Config types (`HandlerConfig`, `Allow`, `Limits`, `Resources`), route-parameter extraction, the `EndpointHandler` (`LocalHandlerImpl`), the capability broker traits (`KvStore`, `SqlDb`, `Outbound`, `SecretSource`), limit enforcement, metrics. No engine. |
| `conduit-handler-wasm` | Wasmtime component path: `wasmtime-wasi`, `wasmtime-wasi-http`, `wasi:keyvalue`/`wasi:config` hosts bound to the broker, `conduit:sqlite` WIT. |
| `conduit-handler-rhai` | `fn handle(req)` runner on the existing Rhai engine settings, the capability modules, the blocking pool. Shares the AST cache code with `conduit-script-rhai` where that is not a coupling. |
| `conduit-kv` | KV backends: `memory`, `redb` (feature), `redis` (feature). |
| `conduit-sql` | SQLite (feature), later Postgres (feature). |

Cargo features (names are proposals):

<!-- docs-check: skip (TOML, not a config) -->
```toml
handler-wasm = ["dep:lopatnov-conduit-handler-wasm"]
handler-rhai = ["dep:lopatnov-conduit-handler-rhai"]
kv-redb      = ["lopatnov-conduit-kv/redb"]
sqlite       = ["lopatnov-conduit-sql/sqlite"]
webapp       = ["handler-wasm", "handler-rhai", "kv-redb", "sqlite", "static", "compression", "hotReload"]
```

- `default` and `standard` do not change: a proxy build pays nothing (decision #31: only heavy things are optional,
  and these are heavy).
- `full` includes `webapp`, so the `:latest` image and the plain release download can serve handlers.
- **`conduit-webapp` as a product:** a release artifact built with `--features webapp` (plus TLS/ACME), published next
  to `-standard`, for people who want "Conduit as an application server" without the rest of `full`. Whether it is a
  separate download or only a documented feature set is a decision once its size is measured.

### Rhai and WASM parity

| | Rhai handler | WASM handler (option B) |
| --- | --- | --- |
| Authoring | edit a `.rhai` file, no build | any language with a WASI HTTP toolchain |
| Request body | whole, bounded | stream, bounded |
| Response body | whole | stream |
| Route data | `req.params`, `req.rest`, `req.route` | `conduit-*` headers (later a WIT interface) |
| KV, files, secrets, config | same capabilities, Rhai modules | same capabilities, WASI interfaces |
| Outbound HTTP | blocking call on the handler pool | async, does not hold a thread |
| SQL | `sql::*` | `conduit:sqlite` |
| CPU limit | operations | fuel |
| Wall-clock limit | `on_progress` deadline | epoch deadline |
| Memory limit | string/array/map sizes | linear memory |
| Portability | Conduit only | Spin, wasmCloud, other `wasi:http` hosts (except `conduit:*` imports) |
| Fits | small endpoints, glue, admin pages, prototypes | real applications, performance, existing code |

The same `allow:` grants and the same `limits:` apply to both; a capability is added to both engines in the same PR,
so the two do not drift (the middleware guides already had to document Rhai's missing request-header write and client
IP; handlers should not repeat that).

---

## Phased plan

Each phase is one PR with its own security pass, tests in the e2e network (`scripts/e2e/`), docs and schema.

1. **Routing and the stateless handler.** `{name}`/`{*name}` path patterns; `handler:` action; `conduit-handler`
   with request/response plumbing, limits, fail-closed errors, metrics; Rhai handlers with no capabilities except
   `config`, `log`, `json`, `time`; WASM `wasi:http` 0.2 components with clocks, random, stdout/stderr, `wasi:config`.
   Measure: binary size of `handler-wasm` (Footprint report), latency and instructions per request of a hello-world
   handler against a static file (the cachegrind harness from #636). Acceptance: a Rust, a Python
   (`componentize-py`) and a JavaScript (`jco`) hello-world component pass the same e2e test.
2. **State.** `global.resources`; KV (`memory`, `redb`, `redis`) through `wasi:keyvalue` and `kv::*`; read-only files;
   secrets; `identity` grant. `lawyer` review of `redb`.
3. **Outbound and SQL.** `fetch` with the allowlist, SSRF checks and `upstream:` targets; SQLite through
   `conduit:sqlite` and `sql::*`; the `webapp` bundle and, if the size allows, the `conduit-webapp` artifact.
   `lawyer` review of `rusqlite`/SQLite.
4. **Later, on demand.** WASI 0.3 components and instance reuse; Postgres; a `conduit:handler/request-context` WIT;
   streaming request bodies to Rhai; a `conduit new handler` scaffold command.

---

## Decisions for the owner

| # | Question | Options | Recommendation |
| --- | --- | --- | --- |
| 1 | WASM interface for handlers | A: extend the current ABI · B: WASI HTTP components · C: B for handlers, A stays for middleware | **C** |
| 2 | Rhai handlers in phase 1? | with WASM in phase 1 · after WASM | **phase 1** (cheap, and it keeps parity from the start) |
| 3 | A broken handler | fail closed (`500`) · fail open (`404`/fall through to the next route) | **fail closed** |
| 4 | Path parameter syntax | `{id}`/`{*rest}` (Axum 0.8) · `:id`/`...` (Spin) | **`{id}`/`{*rest}`** |
| 5 | Route data to WASM | reserved headers · a WIT interface · both | **headers first, WIT later** |
| 6 | Where resources are declared | `global.resources` · per site · both | **`global.resources`** (a store shared by two sites is common) |
| 7 | `conduit-webapp` | separate release artifact · only a feature set in `full` | **decide after the phase-1 size measurement** |
| 8 | Phase-1 scope | stateless only (as above) · stateless plus KV | **stateless only**: the routing and fail-closed plumbing is the part that touches the hot path |

## What is not known yet

- **Binary size** of `component-model` + `async` + `wasmtime-wasi` + `wasmtime-wasi-http`. Not measured; phase 1
  measures it before anything is added to `full`.
- **Per-request cost** of instantiating a component from an instance-pre in Wasmtime 50, compared with today's core
  module path (which also builds a new `Linker` per call; caching it is a middleware optimisation of its own).
- Whether `responseTransform` and `X-Response-Time` should apply to handler responses (they run in the proxy response
  chain today, which a local handler bypasses).
- Which language toolchains emit `wasi:http` 0.3 components today, which decides when 0.3 support is worth adding.
- Whether `jco`/ComponentizeJS components (which embed a JS engine, several MiB) fit the default `memoryMiB` and
  `timeoutMs`; phase 1's three-language test answers it.
- How this relates to #187 (QuickJS middleware): a JS developer may prefer a QuickJS *handler* to a `jco` component.
  If #187 is implemented, adding `handler: { js: ... }` on the same plumbing is cheap.
