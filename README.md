# Conduit

[![CI](https://github.com/lopatnov/conduit/actions/workflows/ci.yml/badge.svg)](https://github.com/lopatnov/conduit/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![npm](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**High-performance reverse proxy and static file server** built on [Cloudflare Pingora](https://github.com/cloudflare/pingora).

- One JSON file describes your entire server — no DSL, no YAML, no Caddyfile
- Serves static files, proxies to backends, terminates TLS, and load-balances — all in one binary
- Drop-in replacement for `express-reverse-proxy` with a fraction of the memory and latency
- Hot-reload in development, auto-TLS (Let's Encrypt) in production

```bash
npx @lopatnov/conduit        # run without installing
cargo install lopatnov-conduit
```

---

## Table of Contents

- [Quick Start](#quick-start)
- [Installation](#installation)
- [Building from Source](#building-from-source)
- [CLI Commands](#cli-commands)
- [Configuration](#configuration)
  - [port / host](#port--host)
  - [tls](#tls)
  - [http2](#http2)
  - [logging](#logging)
  - [compression](#compression)
  - [responseTime](#responsetime)
  - [securityHeaders](#securityheaders)
  - [cors](#cors)
  - [ipFilter](#ipfilter)
  - [limits](#limits)
  - [rateLimit](#ratelimit)
  - [basicAuth](#basicauth)
  - [headers](#headers)
  - [redirects](#redirects)
  - [static / staticOptions](#static--staticoptions)
  - [hotReload](#hotreload)
  - [proxy](#proxy)
  - [upload](#upload)
  - [healthCheck](#healthcheck)
  - [metrics](#metrics)
  - [fallback](#fallback)
  - [Multi-site (global + sites)](#multi-site-global--sites)
- [Configuration Recipes](#configuration-recipes)
- [Admin API](#admin-api)
- [Docker](#docker)
- [Benchmarks](#benchmarks)
- [Contributing](#contributing)
- [License](#license)

---

## Quick Start

```bash
# 1. Create a config with the interactive wizard
conduit init

# 2. Start
conduit

# 3. Validate before deploying to production
conduit validate
```

Minimum working config — save as `conduit.json`:

```json
{
  "port": 3000,
  "static": "./dist",
  "proxy": { "/api": "http://localhost:4000" }
}
```

```
GET /            → serves ./dist/index.html
GET /style.css   → serves ./dist/style.css
GET /api/users   → proxied to http://localhost:4000/api/users
```

---

## Installation

### npx — no install required

```bash
npx @lopatnov/conduit
npx @lopatnov/conduit -c my-config.json
npx @lopatnov/conduit validate
```

### npm — global install

```bash
npm install -g @lopatnov/conduit
conduit
```

### Cargo

```bash
cargo install lopatnov-conduit
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/lopatnov/conduit/releases):

| Platform | File |
|---|---|
| Linux x86-64 | `conduit-x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86-64 musl (Docker) | `conduit-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64 | `conduit-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Intel | `conduit-x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon | `conduit-aarch64-apple-darwin.tar.gz` |
| Windows x86-64 | `conduit-x86_64-pc-windows-msvc.exe.zip` |

```bash
# Linux example
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz
./conduit --version
```

---

## Building from Source

### Prerequisites

- Rust stable toolchain: [rustup.rs](https://rustup.rs)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Build

```bash
git clone https://github.com/lopatnov/conduit
cd conduit

# Debug build — fast to compile, slower to run
cargo build
./target/debug/conduit --version

# Release build — optimized, ~15 MB stripped binary
cargo build --release
./target/release/conduit --version
# Windows: target\release\conduit.exe
```

### Cross-compilation

Cross-compilation lets you build a Linux binary on Windows, or a musl binary for Docker.
It uses [cross](https://github.com/cross-rs/cross), which requires Docker.

```bash
# Install cross
cargo install cross

# Make sure Docker is running, then:

# Linux musl — smallest binary, runs in Docker FROM scratch
cross build --release --target x86_64-unknown-linux-musl

# Linux ARM64 — for Raspberry Pi, AWS Graviton, etc.
cross build --release --target aarch64-unknown-linux-gnu

# Windows from Linux/macOS
cross build --release --target x86_64-pc-windows-msvc
```

> **macOS targets** (`x86_64-apple-darwin`, `aarch64-apple-darwin`) can only be compiled
> on macOS — cross-compiling from Linux/Windows to macOS requires
> [osxcross](https://github.com/tpoechtrager/osxcross) and Apple SDK licensing is complex.
> The [release workflow](.github/workflows/release.yml) handles macOS targets
> using GitHub-hosted macOS runners.

Output binary location: `target/<target>/release/conduit` (or `.exe` on Windows).

### Release profile

The `Cargo.toml` release profile is set for maximum performance and minimum binary size:

```toml
[profile.release]
lto            = true   # link-time optimization across crates
codegen-units  = 1      # single codegen unit — slower compile, faster binary
strip          = true   # strip debug symbols
```

---

## CLI Commands

```
conduit                         start the server (reads conduit.json)
conduit -c <file>               start with a specific config file
conduit --version               print version and exit

conduit init [-o <file>]        interactive wizard — creates conduit.json
conduit validate [-c <file>]    validate config (exit 0 = OK, exit 1 = errors)
conduit probe [-c <file>]       HEAD to every upstream, show latency table
conduit fmt [-c <file>]         pretty-print config to stdout
conduit fmt --write [-c <file>] pretty-print config back to the file

conduit reload                  apply config changes without restart
conduit status                  show server uptime, version, inflight requests
conduit upstreams               list upstreams with live/down status and latency
conduit shutdown                graceful shutdown (waits for inflight requests)
```

### Dynamic upstream management (in-memory only)

```bash
# Add a new backend — takes effect immediately, lost on restart
conduit upstreams add --route /api --target http://b3:4000

# Add with weight (weighted-round-robin only)
conduit upstreams add --route /api --target http://b3:4000 --weight 2

# Remove a backend
conduit upstreams remove --route /api --target http://b1:4000

# Change weight
conduit upstreams weight --route /api --target http://b1:4000 --weight 5
```

Use `conduit reload` to reset to the config file after dynamic changes.

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `RUST_LOG` | `info` | Log level: `error` `warn` `info` `debug` `trace` |
| `CONDUIT_ADMIN` | `127.0.0.1:2019` | Admin API address for management commands |

---

## Configuration

All options are optional unless noted. Fields accept environment variable references
(`"$VAR"` is replaced with the value of `VAR` at startup).

Conduit reads `conduit.json` by default. Pass `-c path/to/file.json` to use another file.

---

### `port` / `host`

Which port and hostname to listen on.

```json
{ "port": 8080 }
```

```json
{ "host": "app.example.com", "port": 443 }
```

`host` is used for virtual hosting — only requests with a matching `Host` header are
handled by this site. Omit `host` to match any hostname (catch-all).

Default port: `3000`.

---

### `tls`

Terminate HTTPS. Two modes: manual certificates or automatic Let's Encrypt.

**Manual certificates** — you supply the PEM files:

```json
{
  "port": 443,
  "tls": {
    "cert": "./certs/cert.pem",
    "key":  "./certs/key.pem"
  }
}
```

**Redirect plain HTTP to HTTPS** — add `httpRedirectPort`:

```json
{
  "port": 443,
  "tls": {
    "cert": "./certs/cert.pem",
    "key":  "./certs/key.pem",
    "httpRedirectPort": 80
  }
}
```

Requests to port 80 are permanently redirected to `https://` on port 443.

**Auto-TLS via Let's Encrypt** (Phase 3) — no cert files needed:

```json
{
  "host": "app.example.com",
  "port": 443,
  "tls": {
    "acme": {
      "email":   "admin@example.com",
      "storage": "./certs",
      "challenge": "http-01"
    }
  }
}
```

Conduit obtains and renews the certificate automatically.
`challenge` can be `"http-01"` (default) or `"tls-alpn-01"`.

> Conduit uses **rustls** — not OpenSSL. TLS version strings use rustls format
> (`"TLSv1.3"`), not OpenSSL format (`"TLSv1.3"` is the same, but cipher names differ).

---

### `http2`

Enable HTTP/2. Requires TLS (browsers only negotiate H2 over HTTPS).

```json
{
  "port": 443,
  "tls": { "cert": "./certs/cert.pem", "key": "./certs/key.pem" },
  "http2": { "maxConcurrentStreams": 250 }
}
```

With `"http2": true` the defaults are used:

```json
{ "http2": true }
```

| Field | Default | Description |
|---|---|---|
| `maxConcurrentStreams` | `100` | Max parallel streams per connection |
| `initialWindowSize` | `65535` | Flow control window (bytes) |

---

### `logging`

Log incoming requests. Accepts `false`, `true`, a format name, or an object.

```json
{ "logging": false }
```

```json
{ "logging": true }
```

```json
{ "logging": "dev" }
```

Available format names:

| Format | Output |
|---|---|
| `"combined"` | Apache combined log (default) |
| `"common"` | Apache common log |
| `"dev"` | Colored, compact — good for terminals |
| `"short"` | Method + URL + status + time |
| `"json"` | Structured JSON — good for log aggregators |

Write to a file:

```json
{ "logging": { "format": "json", "file": "./logs/access.log" } }
```

The file is atomically swapped on `conduit reload` — no log lines are lost.

---

### `compression`

Compress responses with gzip and/or brotli. Accepts `false`, `true`, or an object.

```json
{ "compression": true }
```

```json
{
  "compression": {
    "algorithms": ["br", "gzip"],
    "level": 6,
    "minBytes": 1024
  }
}
```

| Field | Default | Description |
|---|---|---|
| `algorithms` | `["br", "gzip"]` | Compression algorithms, in preference order |
| `level` | `6` | Compression level (1 = fast, 9 = best) |
| `minBytes` | `1024` | Skip compression for responses smaller than this |

Conduit reads the client's `Accept-Encoding` header and picks the best matching algorithm.

---

### `responseTime`

Add an `X-Response-Time` header showing how long the server took to respond.

```json
{ "responseTime": true }
```

```json
{ "responseTime": { "digits": 3 } }
```

`digits` controls decimal places in the millisecond value. Default: `3` (e.g., `1.234ms`).

---

### `securityHeaders`

Add a standard set of security headers to every response.

```json
{ "securityHeaders": true }
```

Headers added:

| Header | Value |
|---|---|
| `Strict-Transport-Security` | `max-age=31536000; includeSubDomains` |
| `X-Content-Type-Options` | `nosniff` |
| `X-Frame-Options` | `SAMEORIGIN` |
| `Referrer-Policy` | `strict-origin-when-cross-origin` |
| `Content-Security-Policy` | `default-src 'self'` |

Fine-tune individual headers with the object form:

```json
{
  "securityHeaders": {
    "contentSecurityPolicy": "default-src 'self'; img-src *"
  }
}
```

---

### `cors`

Handle CORS preflight requests and add `Access-Control-*` headers. Accepts `false`, `true`,
or an object.

```json
{ "cors": true }
```

`true` allows any origin. For production, restrict to specific origins:

```json
{
  "cors": {
    "origins": ["https://app.example.com", "https://www.example.com"],
    "methods": ["GET", "POST", "PUT", "DELETE", "OPTIONS"],
    "allowedHeaders": ["Content-Type", "Authorization"],
    "credentials": true,
    "maxAgeSecs": 86400
  }
}
```

| Field | Default | Description |
|---|---|---|
| `origins` | `["*"]` | Allowed origins |
| `methods` | all | Allowed methods |
| `allowedHeaders` | all | Allowed request headers |
| `credentials` | `false` | Allow cookies / auth headers |
| `maxAgeSecs` | `86400` | How long to cache preflight response |

---

### `ipFilter`

Allow or deny requests by IP address. Applied before auth and rate limiting.

**Whitelist** — allow only specific IPs/ranges, block everything else:

```json
{
  "ipFilter": {
    "allow": ["10.0.0.0/8", "192.168.0.0/16", "203.0.113.5"]
  }
}
```

**Blacklist** — block specific IPs, allow everything else:

```json
{
  "ipFilter": {
    "deny": ["1.2.3.4", "5.6.7.0/24"]
  }
}
```

**Behind another proxy** — read IP from `X-Forwarded-For`:

```json
{
  "ipFilter": {
    "deny": ["1.2.3.4"],
    "trustProxy": true
  }
}
```

Blocked requests receive `403 Forbidden`. Health and metrics endpoints are not affected.
IPv4, IPv6, and IPv4-mapped IPv6 addresses (`::ffff:1.2.3.4`) are all supported.

---

### `limits`

Reject oversized requests before they reach the proxy or handlers.

```json
{
  "limits": {
    "maxBodyBytes": 1048576,
    "maxHeaderBytes": 8192,
    "timeoutSecs": 30
  }
}
```

| Field | Description | Status code |
|---|---|---|
| `maxBodyBytes` | Max request body size | `413 Request Entity Too Large` |
| `maxHeaderBytes` | Max total header size | `431 Request Header Fields Too Large` |
| `timeoutSecs` | Max request duration | `408 Request Timeout` |

---

### `rateLimit`

Token-bucket rate limiter. Keyed by client IP by default.

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 100
  }
}
```

Clients exceeding the limit receive `429 Too Many Requests`.

**Key by a header** — useful for API keys:

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 1000,
    "keyBy": "header:X-API-Key"
  }
}
```

**Exclude paths** from rate limiting:

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 100,
    "skipPaths": ["/__health__", "/__metrics__", "/public/**"]
  }
}
```

| Field | Default | Description |
|---|---|---|
| `windowSecs` | required | Time window in seconds |
| `limit` | required | Max requests per window |
| `algorithm` | `"token-bucket"` | Rate limit algorithm |
| `keyBy` | `"ip"` | `"ip"` or `"header:X-Name"` |
| `skipPaths` | `[]` | Paths exempt from limiting (glob patterns) |

---

### `basicAuth`

Require HTTP Basic Authentication.

```json
{
  "basicAuth": {
    "users": {
      "alice": "secret123",
      "bob": "$BOB_PASSWORD"
    }
  }
}
```

**Use a `$VAR` reference** to avoid storing passwords in the config file.
Conduit resolves environment variables at startup.

Show a browser login dialog:

```json
{
  "basicAuth": {
    "users": { "admin": "$ADMIN_PASSWORD" },
    "challenge": true,
    "realm": "Admin Area"
  }
}
```

Skip auth for public paths:

```json
{
  "basicAuth": {
    "users": { "admin": "$ADMIN_PASSWORD" },
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

Health (`/__health__`) and metrics (`/__metrics__`) endpoints always bypass auth.

---

### `headers`

Add custom headers to every response from this site.

```json
{
  "headers": {
    "X-Powered-By": "Conduit",
    "Cache-Control": "no-store",
    "X-Request-ID": "generated-per-request"
  }
}
```

Set a header to `""` to remove it from the response:

```json
{ "headers": { "Server": "" } }
```

---

### `redirects`

Redirect paths with 3xx responses. First matching rule wins.

```json
{
  "redirects": [
    { "from": "/old-page",   "to": "/new-page",       "status": 301 },
    { "from": "/blog/:slug", "to": "/posts/:slug",     "status": 308 },
    { "from": "/docs",       "to": "https://docs.example.com", "status": 302 }
  ]
}
```

`:param` captures a path segment and substitutes it in `to`.

| Status | Meaning |
|---|---|
| `301` | Moved Permanently (GET stays GET) |
| `302` | Found — temporary redirect |
| `307` | Temporary Redirect (method preserved) |
| `308` | Permanent Redirect (method preserved) |

---

### `static` / `staticOptions`

Serve files from a directory.

**Simple** — serve everything from one directory:

```json
{ "static": "./dist" }
```

**Multiple directories** — searched in order:

```json
{ "static": ["./dist", "./public"] }
```

**Map URL prefixes to directories**:

```json
{
  "static": {
    "/":       "./dist",
    "/assets": "./assets"
  }
}
```

**Static options:**

```json
{
  "static": "./dist",
  "staticOptions": {
    "etag":          true,
    "lastModified":  true,
    "maxAge":        "7d",
    "index":         ["index.html"],
    "dotFiles":      "ignore",
    "preCompressed": true
  }
}
```

| Field | Default | Description |
|---|---|---|
| `etag` | `true` | Generate ETag headers (enables 304 Not Modified) |
| `lastModified` | `true` | Set Last-Modified header |
| `maxAge` | `"0"` | Cache-Control max-age (`"1h"`, `"7d"`, `"1y"`) |
| `index` | `["index.html"]` | Directory index filenames |
| `dotFiles` | `"ignore"` | `"ignore"` \| `"allow"` \| `"deny"` |
| `preCompressed` | `false` | Serve `.br` / `.gz` variant if the client supports it |

---

### `hotReload`

Inject a browser hot-reload script — the page refreshes automatically when watched files change.

```json
{ "hotReload": true }
```

Watch only specific file extensions:

```json
{
  "hotReload": {
    "extensions": [".html", ".css", ".js", ".ts", ".json"]
  }
}
```

Conduit serves a small SSE endpoint at `/__hot-reload__` and injects a `<script>` tag
that listens for file-change events and calls `location.reload()`.

Works best combined with `staticOptions.etag: false` to prevent stale caching in dev:

```json
{
  "hotReload": true,
  "staticOptions": { "etag": false, "lastModified": false }
}
```

---

### `proxy`

Proxy requests to one or more backends.

**Simple** — proxy everything to one backend:

```json
{ "proxy": "http://localhost:4000" }
```

**Route-based** — match URL prefix, proxy to backend:

```json
{
  "proxy": {
    "/api":    "http://api-server:4000",
    "/images": "http://image-server:5000"
  }
}
```

**Round-robin across multiple backends**:

```json
{
  "proxy": {
    "/api": ["http://b1:4000", "http://b2:4000", "http://b3:4000"]
  }
}
```

**Full form** — load balancing, health checks, caching, retries:

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://b1:4000", "http://b2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "healthCheck": {
        "path": "/health",
        "intervalSecs": 10,
        "unhealthyThreshold": 2
      },
      "retry": {
        "attempts": 3,
        "conditions": ["connection_error", "5xx"],
        "backoffMs": 100
      },
      "cache": {
        "store": "memory",
        "maxSizeMb": 256,
        "ttlSecs": 300,
        "skipIfCookie": true
      }
    }
  }
}
```

**Weighted round-robin** — send more traffic to powerful servers:

```json
{
  "proxy": {
    "/api": {
      "targets": [
        { "url": "http://big-server:4000",   "weight": 3 },
        { "url": "http://small-server:4000", "weight": 1 }
      ],
      "strategy": "weighted-round-robin"
    }
  }
}
```

**Sticky sessions** — same client always goes to the same backend:

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://b1:4000", "http://b2:4000"],
      "strategy": "ip-hash",
      "hashKey": "ip"
    }
  }
}
```

**Load balancing strategies:**

| Strategy | Description |
|---|---|
| `round-robin` | Cycle through backends in order (default) |
| `weighted-round-robin` | Cycle with weights — needs `WeightedTarget` objects |
| `random` | Pick a random backend |
| `least-conn` | Pick the backend with fewest active connections |
| `least-response-time` | Pick the fastest backend (measured in background) |
| `ip-hash` | Hash client IP → same client, same backend |
| `consistent-hash` | Ketama hash — minimal reshuffling when backends change |

**`stripPrefix`** — remove the route prefix before forwarding:

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "stripPrefix": true
    }
  }
}
```

`GET /api/users` → forwarded as `GET /users` to the backend.

**Proxy cache** — cache backend responses in memory:

```json
{
  "proxy": {
    "/api/public": {
      "targets": ["http://backend:4000"],
      "cache": {
        "store": "memory",
        "maxSizeMb": 256,
        "ttlSecs": 300,
        "varyHeaders": ["Accept-Language"],
        "skipIfCookie": true,
        "skipPaths": ["/api/public/auth/**"],
        "methods": ["GET", "HEAD"]
      }
    }
  }
}
```

| Field | Default | Description |
|---|---|---|
| `store` | required | `"memory"` \| `"disk:./cache"` \| `"redis://..."` |
| `maxSizeMb` | unlimited | Max cache size |
| `ttlSecs` | from response | Override cache TTL |
| `varyHeaders` | `[]` | Include these headers in the cache key |
| `skipIfCookie` | `false` | Never cache responses to requests with cookies |
| `skipPaths` | `[]` | Glob patterns to never cache |
| `methods` | `["GET","HEAD"]` | Which HTTP methods to cache |

---

### `upload`

Accept file uploads at a given path and save them to disk.

```json
{
  "upload": {
    "path": "/upload",
    "dir":  "./uploads"
  }
}
```

With limits and type filtering:

```json
{
  "upload": {
    "path": "/upload",
    "dir": "./uploads",
    "maxFileSizeBytes":  10485760,
    "maxTotalSizeBytes": 52428800,
    "maxFiles": 5,
    "allowedMimeTypes": ["image/jpeg", "image/png", "image/webp"]
  }
}
```

Upload a file:

```bash
curl -F "file=@photo.jpg" http://localhost:3000/upload
```

Response:

```json
{ "files": [{ "name": "a3f2c1d0-....jpg", "size": 204800 }] }
```

Files are saved with UUID v4 names to prevent collisions and path traversal.

| Field | Description |
|---|---|
| `path` | URL path that accepts `POST multipart/form-data` |
| `dir` | Directory where files are saved |
| `maxFileSizeBytes` | Reject individual files larger than this |
| `maxTotalSizeBytes` | Reject the whole request if total exceeds this |
| `maxFiles` | Max number of files per request |
| `allowedMimeTypes` | Whitelist of accepted MIME types |
| `fieldName` | Form field name (default: any) |

---

### `healthCheck`

Expose a health check endpoint.

```json
{ "healthCheck": true }
```

Default path: `/__health__`. Always returns `200 OK` with a JSON body:

```json
{ "status": "ok", "uptime": 3600, "version": "0.1.0" }
```

Custom path:

```json
{ "healthCheck": { "path": "/health" } }
```

The health endpoint **bypasses auth, rate limiting, and IP filtering** — it is always
reachable for load balancer probes.

---

### `metrics`

Expose a [Prometheus](https://prometheus.io) metrics endpoint.

```json
{ "metrics": { "path": "/__metrics__" } }
```

Secure it with a Bearer token:

```json
{ "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" } }
```

Requests without `Authorization: Bearer <token>` receive `401 Unauthorized`.

Metrics exposed:

| Metric | Type | Description |
|---|---|---|
| `conduit_requests_total` | counter | Total requests, by status and route |
| `conduit_request_duration_seconds` | histogram | Request latency |
| `conduit_inflight_requests` | gauge | Currently active requests |
| `conduit_upstream_health` | gauge | 1 = healthy, 0 = down, per upstream |
| `conduit_cache_hits_total` | counter | Proxy cache hits |
| `conduit_cache_misses_total` | counter | Proxy cache misses |

---

### `fallback`

Return a response when nothing else matched — useful for SPAs.

**SPA fallback** — serve `index.html` for all unknown paths:

```json
{ "fallback": { "status": 200, "file": "./dist/index.html" } }
```

**JSON 404** — return a JSON error body:

```json
{ "fallback": { "status": 404, "body": { "error": "Not Found" } } }
```

**Content-type aware** — serve HTML for browsers, JSON for API clients:

```json
{
  "fallback": {
    "byAccept": {
      "html": { "status": 200, "file": "./dist/index.html" },
      "json": { "status": 404, "body": { "error": "Not Found" } },
      "*":    { "status": 200, "file": "./dist/index.html" }
    }
  }
}
```

Extra headers on the fallback response:

```json
{
  "fallback": {
    "status": 200,
    "file": "./dist/index.html",
    "headers": { "Cache-Control": "no-store" }
  }
}
```

---

### Multi-site (`global` + `sites`)

Run multiple virtual hosts from one Conduit process.

```json
{
  "global": {
    "workers": 4,
    "shutdownTimeoutSecs": 30,
    "admin": { "bind": "127.0.0.1:2019" }
  },
  "sites": [
    {
      "host": "app.example.com",
      "port": 443,
      "tls": { "acme": { "email": "ops@example.com" }, "httpRedirectPort": 80 },
      "static": "./dist",
      "proxy": { "/api": "http://api:4000" }
    },
    {
      "host": "admin.example.com",
      "port": 443,
      "tls": { "cert": "$CERT", "key": "$KEY" },
      "basicAuth": { "users": { "admin": "$ADMIN_PASS" }, "challenge": true },
      "static": "./admin-ui"
    },
    {
      "host": "*",
      "port": 443,
      "tls": { "cert": "$CERT", "key": "$KEY" },
      "fallback": { "status": 404, "body": "Unknown host" }
    }
  ]
}
```

Requests are matched by `Host` header. The catch-all `"*"` site handles anything
that doesn't match a named host. Sites are evaluated in order.

**Config forms** — three equivalent ways to write a config:

```json
// Single site (most common)
{ "port": 3000, "static": "./dist" }

// Array of sites
[
  { "host": "a.com", "port": 443 },
  { "host": "b.com", "port": 443 }
]

// Full form with global settings
{ "global": { "workers": 4 }, "sites": [...] }
```

---

## Configuration Recipes

### SPA with API proxy (production)

```json
{
  "host": "app.example.com",
  "port": 443,
  "tls": { "acme": { "email": "ops@example.com" }, "httpRedirectPort": 80 },
  "http2": true,
  "compression": true,
  "securityHeaders": true,
  "logging": { "format": "json", "file": "/var/log/conduit/access.log" },
  "static": "./dist",
  "staticOptions": { "maxAge": "7d", "preCompressed": true },
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "healthCheck": { "path": "/health", "intervalSecs": 10 },
      "cache": { "store": "memory", "ttlSecs": 60, "skipIfCookie": true }
    }
  },
  "rateLimit": { "windowSecs": 60, "limit": 300, "skipPaths": ["/__health__"] },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" },
  "fallback": {
    "byAccept": {
      "html": { "status": 200, "file": "./dist/index.html" },
      "json": { "status": 404, "body": { "error": "Not Found" } },
      "*":    { "status": 200, "file": "./dist/index.html" }
    }
  }
}
```

### Frontend development

```json
{
  "port": 3000,
  "logging": "dev",
  "cors": true,
  "hotReload": { "extensions": [".html", ".css", ".js", ".ts", ".json"] },
  "static": "./src",
  "staticOptions": { "etag": false, "lastModified": false },
  "proxy": { "/api": "http://localhost:4000" },
  "fallback": { "status": 200, "file": "./src/index.html" }
}
```

### Microservices gateway

```json
{
  "port": 8080,
  "logging": "combined",
  "ipFilter": { "allow": ["10.0.0.0/8"] },
  "proxy": {
    "/users":   "http://users-svc:4001",
    "/orders":  "http://orders-svc:4002",
    "/catalog": {
      "targets": ["http://catalog1:4003", "http://catalog2:4003"],
      "strategy": "round-robin",
      "healthCheck": { "path": "/health", "intervalSecs": 5 }
    }
  },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__" }
}
```

### File upload service

```json
{
  "port": 3000,
  "ipFilter": { "allow": ["10.0.0.0/8"] },
  "limits": { "maxBodyBytes": 52428800 },
  "upload": {
    "path": "/upload",
    "dir": "./uploads",
    "maxFileSizeBytes": 10485760,
    "maxFiles": 10,
    "allowedMimeTypes": ["image/jpeg", "image/png", "image/webp", "application/pdf"]
  },
  "static": { "/files": "./uploads" },
  "healthCheck": true
}
```

---

## Admin API

The Admin API runs on `127.0.0.1:2019` (loopback only, never exposed to the network).
Change the address in the `global` config:

```json
{ "global": { "admin": { "bind": "127.0.0.1:2019" } } }
```

Or override for a single command:

```bash
conduit reload --admin 127.0.0.1:2019
```

| Endpoint | Method | Description |
|---|---|---|
| `/status` | GET | Server version, uptime, inflight requests |
| `/reload` | POST | Apply config changes (hot-reload) |
| `/shutdown` | POST | Graceful shutdown |
| `/upstreams` | GET | All upstreams with health and latency |
| `/upstreams/add` | POST | Add upstream in memory |
| `/upstreams/remove` | POST | Remove upstream from memory |
| `/upstreams/weight` | POST | Change upstream weight |

**What `reload` can and cannot change without restart:**

| Can reload | Cannot reload (requires restart) |
|---|---|
| `proxy`, `static`, `fallback` | `port` |
| `logging`, `compression`, `cors` | `tls.cert` / `tls.key` |
| `rateLimit`, `basicAuth`, `ipFilter` | `http2.*` |
| `headers`, `redirects`, `limits` | `global.workers` |
| `metrics`, `healthCheck`, `upload` | `global.admin.bind` |

When you try to reload a cold field, Conduit returns an error listing exactly which fields
need a restart.

---

## Docker

### Minimal image (FROM scratch)

```dockerfile
FROM scratch
COPY conduit-x86_64-unknown-linux-musl /conduit
COPY conduit.json /conduit.json
EXPOSE 8080
ENTRYPOINT ["/conduit", "-c", "/conduit.json"]
```

Build the musl binary locally or download it from GitHub Releases, then:

```bash
docker build -t my-conduit .
docker run -p 8080:8080 my-conduit
```

### Official image

```bash
docker run -p 8080:8080 \
  -v $(pwd)/conduit.json:/conduit.json:ro \
  -v $(pwd)/dist:/dist:ro \
  lopatnov/conduit
```

### docker-compose

```yaml
services:
  conduit:
    image: lopatnov/conduit:latest
    ports:
      - "443:443"
      - "80:80"
    volumes:
      - ./conduit.json:/conduit.json:ro
      - ./dist:/dist:ro
      - ./certs:/certs
    environment:
      METRICS_TOKEN: "${METRICS_TOKEN}"
    restart: unless-stopped

  api:
    image: my-api:latest
    expose: ["4000"]
```

---

## Benchmarks

See [BENCHMARKS.md](BENCHMARKS.md).

> **Note:** Published numbers are design targets, not yet measured results.
> Real benchmark runs will be added as the project matures.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

Report bugs: [GitHub Issues](https://github.com/lopatnov/conduit/issues)

---

## License

[Apache 2.0](LICENSE) © [lopatnov](https://github.com/lopatnov)
