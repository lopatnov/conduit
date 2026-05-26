# Conduit

[![CI](https://github.com/lopatnov/conduit/actions/workflows/ci.yml/badge.svg)](https://github.com/lopatnov/conduit/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dt/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![GitHub stars](https://img.shields.io/github/stars/lopatnov/conduit)](https://github.com/lopatnov/conduit/stargazers)
[![GitHub issues](https://img.shields.io/github/issues/lopatnov/conduit)](https://github.com/lopatnov/conduit/issues)
[![License](https://img.shields.io/github/license/lopatnov/conduit)](LICENSE)

**High-performance reverse proxy and static file server** built on [Cloudflare Pingora](https://github.com/cloudflare/pingora).

Serves static files, proxies to backends, terminates TLS, and load-balances — configured with a
single JSON file and packaged as a **single binary with no runtime dependencies**.

```bash
# Try immediately — no installation needed
npx @lopatnov/conduit init   # interactive setup wizard
npx @lopatnov/conduit        # start the server

# Or install globally
npm install -g @lopatnov/conduit
cargo install lopatnov-conduit
```

---

## Table of Contents

- [Quick Start](#quick-start)
- [Live Demo](#live-demo)
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
  - [apiKey](#apikey)
  - [redirects](#redirects)
  - [static / staticOptions](#static--staticoptions)
  - [proxy](#proxy)
  - [routes (advanced routing)](#routes-advanced-routing)
  - [Load balancing](#load-balancing)
  - [Proxy cache](#proxy-cache)
  - [healthCheck](#healthcheck)
  - [upload](#upload)
  - [hotReload](#hotreload)
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

```text
GET /            → serves ./dist/index.html
GET /style.css   → serves ./dist/style.css
GET /api/users   → proxied to http://localhost:4000/api/users
```

---

## Live Demo

The repository includes a self-contained demo with **two virtual sites running from a single
Conduit process**, a round-robin load balancer across two API backends, proxy caching, Basic Auth,
and more — just like [express-reverse-proxy's demo](https://github.com/lopatnov/express-reverse-proxy)
runs on multiple ports, but here everything shares one binary.

```bash
# Terminal 1 — two mock API instances (ports 4000 and 4001)
node demo/api/server.js

# Terminal 2 — Conduit: two virtual sites from one process
conduit -c demo/conduit.json
```

| URL | Description |
| --- | --- |
| [http://localhost:8080](http://localhost:8080) | Public app — proxy, cache, compression, rate limiting |
| [http://localhost:8081](http://localhost:8081) | Admin panel — protected with Basic Auth (`admin / demo1234`) |

**VS Code users:** run the _"Demo: Start (Conduit + API)"_ task (`Terminal → Run Task…`)
to launch both processes at once.

See [`demo/README.md`](demo/README.md) for full details.

---

## Installation

### Option 1 — npx (no installation, always latest)

```bash
npx @lopatnov/conduit
npx @lopatnov/conduit init
npx @lopatnov/conduit validate
```

### Option 2 — npm global install

Install once, run anywhere as `conduit`:

```bash
npm install -g @lopatnov/conduit
conduit
conduit validate
```

### Option 3 — Cargo

```bash
cargo install lopatnov-conduit
```

### Option 4 — Pre-built binaries

Download from [GitHub Releases](https://github.com/lopatnov/conduit/releases):

| Platform                   | File                                       |
| -------------------------- | ------------------------------------------ |
| Linux x86-64               | `conduit-x86_64-unknown-linux-gnu.tar.gz`  |
| Linux x86-64 musl (Docker) | `conduit-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64                | `conduit-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Intel                | `conduit-x86_64-apple-darwin.tar.gz`       |
| macOS Apple Silicon        | `conduit-aarch64-apple-darwin.tar.gz`      |
| Windows x86-64             | `conduit-x86_64-pc-windows-msvc.exe.zip`   |

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

Requires [cross](https://github.com/cross-rs/cross) and Docker:

```bash
cargo install cross

# Linux musl — smallest binary, runs in Docker FROM scratch
cross build --release --target x86_64-unknown-linux-musl

# Linux ARM64 — for Raspberry Pi, AWS Graviton, etc.
cross build --release --target aarch64-unknown-linux-gnu
```

> macOS targets can only be built on macOS. The [release workflow](.github/workflows/release.yml)
> handles them using GitHub-hosted runners.

### Release profile

```toml
[profile.release]
lto            = true   # link-time optimization across crates
codegen-units  = 1      # single codegen unit — slower compile, faster binary
strip          = true   # strip debug symbols
```

---

## CLI Commands

```text
conduit                         start the server (reads conduit.json)
conduit -c <file>               start with a specific config file
conduit --version               print version and exit

conduit init [-o <file>]        interactive wizard — creates conduit.json
conduit validate [-c <file>]    validate config (exit 0 = OK, exit 1 = errors)
conduit probe [-c <file>]       HEAD to every upstream, show latency table
conduit fmt [-c <file>]         pretty-print config to stdout
conduit fmt --write [-c <file>] pretty-print config back to the file

conduit reload [--admin ADDR]   hot-reload config without restarting
conduit status [--admin ADDR]   show server uptime, version, inflight requests
conduit upstreams [--admin ADDR]         list all upstream health and latency
conduit upstreams add    --route PATH --target URL [--weight N]
conduit upstreams remove --route PATH --target URL
conduit upstreams weight --route PATH --target URL --weight N
conduit shutdown [--admin ADDR] graceful shutdown
```

### Shell completions

```bash
conduit completions bash   >> ~/.bashrc
conduit completions zsh    >> ~/.zshrc
conduit completions fish   >> ~/.config/fish/completions/conduit.fish
conduit completions powershell >> $PROFILE
```

### Environment variables

| Variable        | Default          | Description                                      |
| --------------- | ---------------- | ------------------------------------------------ |
| `RUST_LOG`      | `info`           | Log level: `error` `warn` `info` `debug` `trace` |
| `CONDUIT_ADMIN` | `127.0.0.1:2019` | Admin API address for management commands        |

---

## Configuration

All options are optional unless noted. Fields accept environment variable references —
`"$VAR"` is replaced with the value of `VAR` at startup.

Conduit reads `conduit.json` by default. Pass `-c path/to/file.json` to use another file.

---

### `port` / `host`

```json
{ "port": 8080 }
```

```json
{ "host": "app.example.com", "port": 443 }
```

`host` is used for virtual hosting — only requests matching the `Host` header are handled
by this site. Omit `host` to match any hostname (catch-all).

Default port: `3000`.

---

### `tls`

**Manual certificates:**

```json
{
  "port": 443,
  "tls": {
    "cert": "./certs/cert.pem",
    "key": "./certs/key.pem",
    "httpRedirectPort": 80
  }
}
```

**Auto-TLS via Let's Encrypt** (no cert/key needed):

```json
{
  "port": 443,
  "tls": {
    "acme": {
      "email": "admin@example.com",
      "storage": "./certs",
      "challenge": "http-01"
    }
  }
}
```

Conduit automatically obtains and renews certificates. `conduit validate` reports expiry status.

> Conduit uses **rustls** — not OpenSSL.

---

### `http2`

```json
{
  "port": 443,
  "tls": { "cert": "./certs/cert.pem", "key": "./certs/key.pem" },
  "http2": true
}
```

| Field                  | Default | Description                         |
| ---------------------- | ------- | ----------------------------------- |
| `maxConcurrentStreams` | `100`   | Max parallel streams per connection |
| `initialWindowSize`    | `65535` | Flow control window (bytes)         |

---

### `logging`

Accepts `false`, `true`, a format string, or an object.

```json
{ "logging": "dev" }
```

```json
{ "logging": { "format": "combined", "file": "./logs/access.log" } }
```

| Format     | Description                                              |
| ---------- | -------------------------------------------------------- |
| `dev`      | Colorized, short — for development                       |
| `combined` | Apache Combined Log Format — for production              |
| `common`   | Apache Common Log Format                                 |
| `short`    | Short, without timestamps                                |
| `json`     | Structured JSON — for log aggregation (ELK, Loki, etc.) |

---

### `compression`

Accepts `false`, `true`, or an object.

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

Conduit negotiates the best algorithm based on the client's `Accept-Encoding` header.
Brotli is preferred over gzip when the client supports both.

---

### `responseTime`

Adds `X-Response-Time: 1.23ms` to every response.

```json
{ "responseTime": true }
```

```json
{ "responseTime": { "digits": 3 } }
```

---

### `securityHeaders`

```json
{ "securityHeaders": true }
```

Headers added with `true`:

| Header                   | Value                             |
| ------------------------ | --------------------------------- |
| `X-Content-Type-Options` | `nosniff`                         |
| `X-Frame-Options`        | `SAMEORIGIN`                      |
| `Referrer-Policy`        | `strict-origin-when-cross-origin` |
| `X-XSS-Protection`       | `1; mode=block`                   |

Object form for HSTS and CSP:

```json
{
  "securityHeaders": {
    "contentSecurityPolicy": "default-src 'self'; img-src *",
    "hsts": "max-age=31536000; includeSubDomains",
    "frameOptions": "DENY"
  }
}
```

---

### `cors`

Accepts `false`, `true`, or an object.

```json
{ "cors": true }
```

For production, restrict to specific origins:

```json
{
  "cors": {
    "origins": ["https://app.example.com"],
    "methods": ["GET", "POST", "PUT", "DELETE", "OPTIONS"],
    "allowedHeaders": ["Content-Type", "Authorization"],
    "credentials": true,
    "maxAgeSecs": 86400
  }
}
```

CORS preflight (`OPTIONS`) requests bypass auth and rate limiting — browsers send them without
credentials.

---

### `ipFilter`

Applied before auth and rate limiting.

**Whitelist** — allow only these IPs/ranges:

```json
{ "ipFilter": { "allow": ["10.0.0.0/8", "192.168.0.0/16"] } }
```

**Blacklist** — block specific IPs:

```json
{ "ipFilter": { "deny": ["1.2.3.4", "5.6.7.0/24"] } }
```

**Behind another proxy:**

```json
{ "ipFilter": { "deny": ["1.2.3.4"], "trustProxy": true } }
```

Blocked requests receive `403 Forbidden`. IPv4, IPv6, and IPv4-mapped IPv6 are all supported.

---

### `limits`

```json
{
  "limits": {
    "maxBodyBytes": 1048576,
    "maxHeaderBytes": 8192,
    "timeoutSecs": 30
  }
}
```

| Field            | Description                | Status code                           |
| ---------------- | -------------------------- | ------------------------------------- |
| `maxBodyBytes`   | Max request body size      | `413 Request Entity Too Large`        |
| `maxHeaderBytes` | Max total header size      | `431 Request Header Fields Too Large` |
| `timeoutSecs`    | Per-request timeout fallback | applied to all proxy peer timeouts  |

---

### `rateLimit`

Token-bucket rate limiter.

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 100
  }
}
```

**Key by a header** (API key, user ID, etc.):

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 1000,
    "keyBy": "header:X-API-Key"
  }
}
```

**Redis-backed** — shared across multiple Conduit instances:

```json
{
  "rateLimit": {
    "windowSecs": 60,
    "limit": 100,
    "store": "redis://localhost:6379"
  }
}
```

Falls back to in-memory if Redis is unavailable.

| Field        | Default          | Description                                |
| ------------ | ---------------- | ------------------------------------------ |
| `windowSecs` | required         | Time window in seconds                     |
| `limit`      | required         | Max requests per window                    |
| `algorithm`  | `"token-bucket"` | Rate limit algorithm                       |
| `keyBy`      | `"ip"`           | `"ip"` or `"header:X-Name"`                |
| `skipPaths`  | `[]`             | Paths exempt from limiting (glob patterns) |
| `store`      | `"memory"`       | `"memory"` or `"redis://..."`              |

---

### `basicAuth`

```json
{
  "basicAuth": {
    "users": { "alice": "secret123", "bob": "$BOB_PASSWORD" },
    "challenge": true,
    "realm": "My App",
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

Use `$VAR` references to avoid storing passwords in the config file.

---

### `apiKey`

```json
{
  "apiKey": {
    "keys": ["$API_KEY_1", "$API_KEY_2"],
    "header": "X-API-Key",
    "skipPaths": ["/__health__", "/public/**"]
  }
}
```

---

### `redirects`

First matching rule wins. Supports `:param` captures and query string preservation.

```json
{
  "redirects": [
    { "from": "/old-page",    "to": "/new-page",         "status": 301 },
    { "from": "/blog/:slug",  "to": "/posts/:slug",       "status": 308 },
    { "from": "/docs",        "to": "https://docs.example.com", "status": 302 }
  ]
}
```

| Status | Meaning                               |
| ------ | ------------------------------------- |
| `301`  | Moved Permanently                     |
| `302`  | Found — temporary redirect            |
| `307`  | Temporary Redirect (method preserved) |
| `308`  | Permanent Redirect (method preserved) |

---

### `static` / `staticOptions`

**Simple:**

```json
{ "static": "./dist" }
```

**Multiple directories** — searched in order:

```json
{ "static": ["./dist", "./public"] }
```

**Map URL prefixes to directories:**

```json
{ "static": { "/": "./dist", "/assets": "./assets" } }
```

**Options:**

```json
{
  "static": "./dist",
  "staticOptions": {
    "etag": true,
    "lastModified": true,
    "maxAge": "7d",
    "index": ["index.html"],
    "dotFiles": "ignore",
    "preCompressed": true
  }
}
```

`preCompressed: true` serves `.br` / `.gz` variants directly without re-compressing on the fly.

| Field           | Default          | Description                                      |
| --------------- | ---------------- | ------------------------------------------------ |
| `etag`          | `true`           | Generate ETag headers (enables 304 Not Modified) |
| `lastModified`  | `true`           | Set Last-Modified header                         |
| `maxAge`        | `"0"`            | Cache-Control max-age (`"1h"`, `"7d"`, `"1y"`)   |
| `index`         | `["index.html"]` | Directory index filenames                        |
| `dotFiles`      | `"ignore"`       | `"ignore"` \| `"allow"` \| `"deny"`              |
| `preCompressed` | `false`          | Serve `.br`/`.gz` sidecar files                  |

---

### `proxy`

**Simple — proxy everything:**

```json
{ "proxy": "http://localhost:4000" }
```

**Route-based:**

```json
{
  "proxy": {
    "/api": "http://api-server:4000",
    "/images": "http://image-server:5000"
  }
}
```

**Round-robin across multiple backends:**

```json
{
  "proxy": {
    "/api": ["http://b1:4000", "http://b2:4000", "http://b3:4000"]
  }
}
```

**Full form — with all options:**

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://b1:4000", "http://b2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "http2": false,
      "timeout":     { "connectMs": 2000, "readMs": 30000 },
      "healthCheck": { "path": "/health", "intervalSecs": 10 },
      "retry": { "attempts": 3, "conditions": ["connection_error", "5xx"] },
      "cache": { "store": "memory", "ttlSecs": 300 }
    }
  }
}
```

**`stripPrefix`** — `GET /api/users` is forwarded as `GET /users` to the backend.

**Path rewrite** — regex-based, first match wins, applied after `stripPrefix`:

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "rewrite": [
        { "from": "^/v[0-9]+/(.+)$", "to": "/$1" }
      ]
    }
  }
}
```

**Upstream groups** — two-level load balancing:

```json
{
  "proxy": {
    "/api": {
      "groups": [
        { "name": "us-east", "targets": ["http://us1:4000", "http://us2:4000"], "strategy": "least-conn" },
        { "name": "eu-west", "targets": ["http://eu1:4000", "http://eu2:4000"], "strategy": "least-conn" }
      ],
      "groupStrategy": "ip-hash"
    }
  }
}
```

**Retry conditions:**

| Condition          | Description                     |
| ------------------ | ------------------------------- |
| `connection_error` | Upstream is down or unreachable |
| `5xx`              | Upstream returns a 5xx response |
| `timeout`          | Read or write timeout           |

---

### `routes` (advanced routing)

Explicit route table evaluated in order; first match wins.

```json
{
  "routes": [
    {
      "match": { "path": "/api/**", "method": ["POST", "PUT", "PATCH", "DELETE"] },
      "proxy": { "targets": ["http://write-backend:4000"], "strategy": "least-conn" }
    },
    {
      "match": { "path": "/api/**" },
      "proxy": "http://read-backend:4000"
    },
    {
      "match": { "path": "/public/**" },
      "static": "./public"
    }
  ]
}
```

**Match criteria** (all present fields must match):

| Field     | Type                 | Description                                               |
| --------- | -------------------- | --------------------------------------------------------- |
| `path`    | glob string          | `*` — one segment, `**` — any depth. Default: match all. |
| `method`  | `string[]`           | HTTP methods (case-insensitive). Default: match all.      |
| `headers` | `{ name: pattern }`  | Header values (exact string or regex).                    |
| `query`   | `{ param: pattern }` | Query param values (exact or regex).                      |

Backward compatibility: top-level `proxy` and `static` are automatically converted to routes.

---

### Load balancing

Controlled by the `strategy` field inside a `proxy` route.

| Strategy             | Value                   | Description                                              |
| -------------------- | ----------------------- | -------------------------------------------------------- |
| Round-robin          | `round-robin`           | Default. Rotate evenly across all healthy backends.      |
| Weighted round-robin | `weighted-round-robin`  | Respects the `weight` field.                             |
| Random               | `random`                | Pick a backend at random each request.                   |
| Least connections    | `least-conn`            | Send to the backend with the fewest active connections.  |
| Least response time  | `least-response-time`   | Send to the backend with the lowest recent latency.      |
| IP hash              | `ip-hash`               | Sticky sessions — same client IP always hits same backend. |
| Consistent hash      | `consistent-hash`       | Ketama ring — minimal reshuffling when backends change.  |

**Weighted round-robin** requires explicit weights:

```json
{
  "proxy": {
    "/api": {
      "targets": [
        { "url": "http://powerful:4000", "weight": 3 },
        { "url": "http://normal:4000",   "weight": 1 }
      ],
      "strategy": "weighted-round-robin"
    }
  }
}
```

`hashKey` for `ip-hash` / `consistent-hash`: `"ip"` (default), `"url"`, or `"header:X-My-Key"`.

---

### Proxy cache

Cache upstream responses in memory, Redis, or on disk.

```json
{
  "proxy": {
    "/api": {
      "targets": ["http://backend:4000"],
      "cache": {
        "store":        "memory",
        "maxSizeMb":    256,
        "ttlSecs":      300,
        "varyHeaders":  ["Accept-Language"],
        "skipPaths":    ["/api/auth/**", "/api/user/**"],
        "skipIfCookie": true,
        "methods":      ["GET", "HEAD"]
      }
    }
  }
}
```

| `store` value     | Description                          |
| ----------------- | ------------------------------------ |
| `"memory"`        | In-process LRU — fastest, non-shared |
| `"redis://..."`   | Redis — shared across instances      |
| `"disk:./cache"`  | Local filesystem — survives restarts |

---

### `healthCheck`

```json
{ "healthCheck": true }
```

Default path: `/__health__`. Always returns `200 OK`:

```json
{ "status": "ok", "uptime": 3600, "version": "0.3.0" }
```

Include upstream health:

```json
{ "healthCheck": { "path": "/health", "includeUpstreams": true } }
```

The health endpoint **bypasses auth, rate limiting, and IP filtering**.

---

### `upload`

Accept `multipart/form-data` file uploads.

```json
{
  "upload": {
    "path": "/upload",
    "dir": "./uploads",
    "maxFileSizeBytes":  10485760,
    "maxTotalSizeBytes": 52428800,
    "maxFiles": 5,
    "allowedMimeTypes": ["image/jpeg", "image/png", "application/pdf"]
  }
}
```

Uploaded files are saved with UUID-based names and the original extension.

---

### `hotReload`

Browser hot reload via SSE — useful for frontend development.

```json
{ "hotReload": true }
```

```json
{
  "hotReload": {
    "extensions": [".html", ".css", ".js", ".ts"],
    "path": "/__hot-reload__"
  }
}
```

Add to your HTML to auto-reload when files change:

```html
<script src="/__hot-reload__/client.js"></script>
```

---

### `metrics`

Prometheus metrics endpoint.

```json
{ "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" } }
```

Metrics exposed:

| Metric                             | Type      | Description                      |
| ---------------------------------- | --------- | -------------------------------- |
| `conduit_requests_total`           | counter   | Total requests, by method/status |
| `conduit_request_duration_seconds` | histogram | Request latency                  |
| `conduit_cache_hits_total`         | counter   | Proxy cache hits                 |
| `conduit_cache_misses_total`       | counter   | Proxy cache misses               |

---

### `fallback`

Return a response when nothing else matched.

**SPA fallback:**

```json
{ "fallback": { "status": 200, "file": "./dist/index.html" } }
```

**Content-type aware:**

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
      "tls": { "cert": "$CERT", "key": "$KEY", "httpRedirectPort": 80 },
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

**Config forms** — three equivalent ways:

```jsonc
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
  "tls": { "cert": "$CERT", "key": "$KEY", "httpRedirectPort": 80 },
  "http2": true,
  "securityHeaders": true,
  "cors": { "origins": ["https://app.example.com"], "credentials": true },
  "logging": { "format": "json", "file": "/var/log/conduit/access.log" },
  "static": "./dist",
  "staticOptions": { "maxAge": "7d", "preCompressed": true },
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "retry": { "attempts": 3, "conditions": ["connection_error", "5xx"] },
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
      "json": { "status": 404, "body": { "error": "Not Found" } }
    }
  }
}
```

### Production with Auto-TLS (Let's Encrypt)

```json
{
  "port": 443,
  "tls": { "acme": { "email": "admin@example.com" } },
  "compression": true,
  "securityHeaders": true,
  "static": "./dist",
  "proxy": { "/api": { "targets": ["http://api:4000"], "strategy": "least-conn" } },
  "healthCheck": true
}
```

### Frontend development with hot reload

```json
{
  "port": 3000,
  "logging": "dev",
  "cors": true,
  "hotReload": true,
  "static": "./src",
  "proxy": { "/api": "http://localhost:4000" },
  "fallback": { "status": 200, "file": "./src/index.html" }
}
```

### Microservices gateway

```json
{
  "port": 8080,
  "ipFilter": { "allow": ["10.0.0.0/8"] },
  "proxy": {
    "/users":   "http://users-svc:4001",
    "/orders":  "http://orders-svc:4002",
    "/catalog": ["http://catalog1:4003", "http://catalog2:4003"]
  },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__" }
}
```

### Weighted load balancing + IP hash

```json
{
  "port": 443,
  "tls": { "cert": "$CERT", "key": "$KEY" },
  "proxy": {
    "/api": {
      "targets": [
        { "url": "http://powerful:4000", "weight": 3 },
        { "url": "http://normal:4000",   "weight": 1 }
      ],
      "strategy": "weighted-round-robin"
    },
    "/auth": {
      "targets": ["http://auth1:5000", "http://auth2:5000"],
      "strategy": "ip-hash",
      "hashKey": "ip"
    }
  }
}
```

---

## Admin API

Runs on `127.0.0.1:2019` (loopback only — never exposed to the network).

```json
{ "global": { "admin": { "bind": "127.0.0.1:2019" } } }
```

| Endpoint              | Method | Description                               |
| --------------------- | ------ | ----------------------------------------- |
| `/status`             | GET    | Server version, uptime, inflight requests |
| `/reload`             | POST   | Hot-reload config from disk               |
| `/shutdown`           | POST   | Graceful shutdown                         |
| `/upstreams`          | GET    | Health, latency, and weights per backend  |
| `/upstreams/add`      | POST   | Add an upstream (in memory only)          |
| `/upstreams/remove`   | POST   | Remove an upstream                        |
| `/upstreams/weight`   | POST   | Change a backend's weight (WRR only)      |

Dynamic upstream changes survive until `conduit reload` — which resets from the config file.

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

See [BENCHMARKS.md](BENCHMARKS.md) for methodology and results.

---

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

- **Bug reports** → [GitHub Issues](https://github.com/lopatnov/conduit/issues)
- **Security vulnerabilities** → [GitHub Security Advisories](https://github.com/lopatnov/conduit/security/advisories) (do not use public issues)
- **Questions & ideas** → [GitHub Discussions](https://github.com/lopatnov/conduit/discussions)
- **Found it useful?** — a ⭐ on GitHub helps others discover the project

---

## License

[Apache 2.0](LICENSE) © 2024–2026 [Oleksandr Lopatnov](https://github.com/lopatnov)
