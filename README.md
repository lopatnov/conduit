# Conduit

[![CI](https://github.com/lopatnov/conduit/actions/workflows/ci.yml/badge.svg)](https://github.com/lopatnov/conduit/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/conduit-proxy.svg)](https://crates.io/crates/conduit-proxy)
[![npm](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**High-performance reverse proxy and static file server** — powered by [Cloudflare Pingora](https://github.com/cloudflare/pingora).

- **150k+ req/s** for static files · **80k+ req/s** for proxy passthrough
- **Single binary** — no runtime, no dependencies
- **One JSON file** describes your entire server
- **Hot-reload** in dev, **auto-TLS** (Let's Encrypt) in production
- **Drop-in replacement** for `express-reverse-proxy` with 10–20× throughput

```bash
# Install and run
npx @lopatnov/conduit
# or
cargo install conduit-proxy
```

---

## Table of Contents

- [Why Conduit?](#why-conduit)
- [Performance](#performance)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [How It Works](#how-it-works)
- [CLI Commands](#cli-commands)
- [Configuration Reference](#configuration-reference)
- [Configuration Recipes](#configuration-recipes)
- [Docker](#docker)
- [Benchmarks](#benchmarks)
- [Contributing](#contributing)
- [Built With](#built-with)
- [License](#license)

---

## Why Conduit?

| Feature | Nginx | Caddy | Traefik | express-reverse-proxy | **Conduit** |
|---|---|---|---|---|---|
| Language | C | Go | Go | Node.js | **Rust** |
| Config | DSL | Caddyfile/JSON | TOML/YAML | JSON | **JSON + Schema** |
| Admin API | ❌ | ✅ | ✅ | ❌ | ✅ |
| Single binary | ✅ | ✅ | ✅ | ❌ | ✅ |
| Auto-TLS | ❌ | ✅ | ✅ | ❌ | ✅ |
| HTTP/2 upstream | ✅ | ✅ | ✅ | ❌ | ✅ |
| Proxy cache | ✅ | ✅ | ✅ | ❌ | ✅ |
| Hot-reload (dev) | ❌ | ❌ | ❌ | ✅ | ✅ |
| File upload | ❌ | ❌ | ❌ | ✅ | ✅ |
| Prometheus | plugin | plugin | ✅ | ❌ | ✅ |
| `validate` CI check | ❌ | ❌ | ❌ | ❌ | ✅ |
| Upstream health | ❌ | ✅ | ✅ | ❌ | ✅ |
| Load balancing | RR, LC, IPHash | RR | RR, WRR | ❌ | **7 strategies** |
| Dynamic upstreams | ❌ | ❌ | ❌ | ❌ | ✅ |
| IP allow/deny | ✅ | ✅ | ✅ | ❌ | ✅ |
| npx support | ❌ | ❌ | ❌ | ✅ | ✅ |

---

## Performance

Measured on Linux x86-64 (AMD EPYC 7763), `wrk -t8 -c200 -d30s`:

| Scenario | express-reverse-proxy | Conduit |
|---|---|---|
| Static file 1 KB | ~8,000 req/s | **≥ 150,000 req/s** |
| Proxy passthrough | ~6,000 req/s | **≥ 80,000 req/s** |
| P99 proxy latency | ~15 ms | **≤ 2 ms** |
| Memory (idle) | ~60 MB | **≤ 10 MB** |
| Startup time | ~500 ms | **≤ 50 ms** |
| Binary size (musl) | N/A | **≤ 15 MB** |

See [BENCHMARKS.md](BENCHMARKS.md) for full methodology and results.

---

## Installation

### npx (no install required)

```bash
npx @lopatnov/conduit
```

### npm (global install)

```bash
npm install -g @lopatnov/conduit
conduit
```

### Cargo

```bash
cargo install conduit-proxy
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/lopatnov/conduit/releases):

| Platform | Binary |
|---|---|
| Linux x86-64 | `conduit-x86_64-unknown-linux-gnu` |
| Linux x86-64 (musl/Docker) | `conduit-x86_64-unknown-linux-musl` |
| Linux ARM64 | `conduit-aarch64-unknown-linux-gnu` |
| macOS x86-64 | `conduit-x86_64-apple-darwin` |
| macOS Apple Silicon | `conduit-aarch64-apple-darwin` |
| Windows x86-64 | `conduit-x86_64-pc-windows-msvc.exe` |

---

## Quick Start

### 30-second setup

```bash
# Interactive wizard — creates conduit.json
conduit init

# Start the server
conduit
```

### Minimal config

```json
{
  "port": 3000,
  "static": "./dist",
  "proxy": { "/api": "http://localhost:4000" }
}
```

```bash
conduit                        # reads conduit.json by default
conduit -c my-config.json      # or specify a file
```

### Validate before deploying

```bash
conduit validate               # exit 0 = OK, exit 1 = errors with details
```

### Check upstream health

```bash
conduit probe                  # HEAD to every upstream, shows latency
```

---

## How It Works

Every request goes through a single pipeline inside Pingora's async runtime:

```
Incoming request
       │
       ▼
 request_filter
  ├─ inflight counter++
  ├─ IP filter        → 403
  ├─ CORS preflight   → 200 (OPTIONS)
  ├─ /__health__      → 200 JSON  ─────────────────┐
  ├─ /__metrics__     → Prometheus text  ───────────┤  Local handlers
  ├─ /__hot-reload__  → SSE stream  ────────────────┤  (no upstream)
  ├─ Size limits      → 413 / 431                   │
  ├─ Rate limit       → 429                         │
  ├─ Auth             → 401                         │
  ├─ Custom headers   (planned)                     │
  ├─ Redirects        → 3xx  ───────────────────────┤
  ├─ Router           → static / proxy / upload     │
  └─ handle_local()  ──────────────────────────────┘
       │
       │  (proxy / upload only)
       ▼
 upstream_peer()      ← load balancer selects target
       │
       ▼
 upstream_request_filter
  ├─ strip prefix
  └─ X-Forwarded-For / X-Forwarded-Proto
       │
       ▼
 upstream (backend responds)
       │
       ▼
 upstream_response_filter
  ├─ cache store (if cacheable)
  ├─ compression (gzip / brotli)
  ├─ X-Response-Time header
  ├─ inflight counter--
  └─ Prometheus metrics
       │
       ▼
   Client receives response
```

**Static files** are served directly in Pingora's hot path at 150k+ req/s — no IPC, no extra process.

**File uploads** go to a loopback Axum server (`127.0.0.1:0`), which handles `multipart/form-data` via `multer`. The port is chosen by the OS and is not configurable.

**Admin API** runs on a separate Axum server (default `127.0.0.1:2019`) and handles `reload`, `shutdown`, and dynamic upstream management.

---

## CLI Commands

```
conduit                                      start the server
conduit -c <file>                            use a specific config file
conduit --version                            print version

conduit init                                 interactive wizard → conduit.json
conduit validate                             validate config (exit 0 = OK)
conduit probe                                HEAD to every upstream, show latency
conduit fmt                                  pretty-print config → stdout
conduit fmt --write                          pretty-print config → file (in place)

conduit reload                               hot-reload config (no restart)
conduit status                               show server status
conduit upstreams                            list all upstreams with health / latency

conduit upstreams add \
  --route /api \
  --target http://b3:4000 \
  --weight 2                                 add upstream in memory only

conduit upstreams remove \
  --route /api \
  --target http://b1:4000                    remove upstream from memory

conduit upstreams weight \
  --route /api \
  --target http://b1:4000 \
  --weight 5                                 change weight (weighted-round-robin only)

conduit shutdown                             graceful shutdown
```

> **Note:** `upstreams add/remove/weight` changes are in-memory only. They are lost on restart.
> Use `conduit reload` to re-apply the config file.

### Options for management commands

```
--admin <ADDR>   Admin API address (default: $CONDUIT_ADMIN or 127.0.0.1:2019)
```

### Environment variables

| Variable | Description |
|---|---|
| `RUST_LOG` | Log level (`error`, `warn`, `info`, `debug`, `trace`) |
| `CONDUIT_ADMIN` | Admin API address (overrides default `127.0.0.1:2019`) |

---

## Configuration Reference

All fields are optional unless marked **required**.

```jsonc
{
  // ── Identity ──────────────────────────────────────────────────────────────
  "version": 1,                        // config schema version
  "host": "app.example.com",           // virtual host (omit for catch-all)
  "port": 8080,                        // listening port (default: 3000)

  // ── TLS ───────────────────────────────────────────────────────────────────
  "tls": {
    // Option A: manual certificates (rustls — not OpenSSL)
    "cert": "./certs/cert.pem",
    "key":  "./certs/key.pem",
    "ca":   "./certs/ca.pem",          // optional client CA
    "httpRedirectPort": 80,            // redirect plain HTTP → HTTPS
    "versions": ["TLSv1.3"],
    "ciphers": ["TLS_AES_256_GCM_SHA384"],

    // Option B: Auto-TLS via Let's Encrypt (cert/key not needed)
    "acme": {
      "email":     "admin@example.com",
      "storage":   "./certs",          // where to persist cert + key
      "challenge": "http-01"           // "http-01" | "tls-alpn-01"
    }
  },

  // ── HTTP/2 ────────────────────────────────────────────────────────────────
  "http2": {
    "maxConcurrentStreams": 100,
    "initialWindowSize": 65535
  },

  // ── Logging ───────────────────────────────────────────────────────────────
  // false | true | "dev" | "json" | "combined" | { ... }
  "logging": { "format": "combined", "file": "./logs/access.log" },

  // ── Compression ───────────────────────────────────────────────────────────
  // false | true | { ... }
  "compression": { "algorithms": ["br", "gzip"], "level": 6, "minBytes": 1024 },

  // ── Response Time ─────────────────────────────────────────────────────────
  // false | true | { "digits": 3 }
  "responseTime": true,

  // ── Security Headers ──────────────────────────────────────────────────────
  // false | true | { ... }
  // Adds: HSTS, X-Content-Type-Options, X-Frame-Options, Referrer-Policy, CSP
  "securityHeaders": true,

  // ── CORS ──────────────────────────────────────────────────────────────────
  // false | true | { ... }
  "cors": {
    "origins": ["https://app.example.com"],
    "methods": ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"],
    "allowedHeaders": ["Content-Type", "Authorization"],
    "credentials": false,
    "maxAgeSecs": 86400
  },

  // ── IP Filter ─────────────────────────────────────────────────────────────
  // If "allow" is set: whitelist mode (everything else → 403)
  // If only "deny" is set: blacklist mode
  "ipFilter": {
    "allow": ["10.0.0.0/8", "192.168.0.0/16"],
    "deny":  ["1.2.3.4"],
    "trustProxy": true                 // use X-Forwarded-For
  },

  // ── Request Limits ────────────────────────────────────────────────────────
  "limits": {
    "maxBodyBytes":   1048576,         // 413 if exceeded
    "maxHeaderBytes": 8192,            // 431 if exceeded
    "timeoutSecs":    30
  },

  // ── Rate Limiting ─────────────────────────────────────────────────────────
  "rateLimit": {
    "windowSecs": 60,
    "limit": 100,
    "algorithm": "token-bucket",
    "keyBy": "ip",                     // "ip" | "header:X-API-Key"
    "skipPaths": ["/__health__", "/__metrics__"]
  },

  // ── Authentication ────────────────────────────────────────────────────────
  "basicAuth": {
    "users": { "admin": "$ADMIN_PASSWORD" },
    "challenge": true,
    "realm": "My App",
    "skipPaths": ["/__health__"]
  },

  // ── Headers ───────────────────────────────────────────────────────────────
  "headers": { "X-Powered-By": "Conduit" },

  // ── Redirects ─────────────────────────────────────────────────────────────
  "redirects": [
    { "from": "/old",        "to": "/new",         "status": 301 },
    { "from": "/blog/:slug", "to": "/posts/:slug",  "status": 308 }
  ],

  // ── Static Files ──────────────────────────────────────────────────────────
  // String | [String] | { "/path": "dir" }
  "static": "./dist",
  "staticOptions": {
    "etag": true,
    "lastModified": true,
    "maxAge": "1d",
    "index": ["index.html"],
    "dotFiles": "ignore",              // "ignore" | "allow" | "deny"
    "preCompressed": false             // serve .br / .gz if present
  },

  // ── Hot Reload (dev) ──────────────────────────────────────────────────────
  // false | true | { ... }
  "hotReload": {
    "extensions": [".html", ".css", ".js", ".ts"]
  },

  // ── Reverse Proxy ─────────────────────────────────────────────────────────
  // Simple: "proxy": "http://localhost:4000"
  // Routes:
  "proxy": {
    "/api": {
      "targets": ["http://b1:4000", "http://b2:4000"],
      // "targets": [
      //   { "url": "http://b1:4000", "weight": 3 },
      //   { "url": "http://b2:4000", "weight": 1 }
      // ],

      // Strategies: round-robin | weighted-round-robin | random
      //             least-conn | least-response-time
      //             ip-hash | consistent-hash
      "strategy": "round-robin",
      "hashKey": "ip",                 // for ip-hash / consistent-hash

      "http2": false,
      "stripPrefix": true,

      "timeout": { "connectMs": 2000, "sendMs": 10000, "readMs": 30000 },
      "pool":    { "maxIdle": 10, "idleTimeoutSecs": 60 },

      "retry": {
        "attempts": 3,
        "conditions": ["connection_error", "5xx"],
        "backoffMs": 100
      },

      "healthCheck": {
        "path": "/health",
        "intervalSecs": 10,
        "unhealthyThreshold": 2,
        "healthyThreshold": 1
      },

      "cache": {
        "store": "memory",             // "memory" | "redis://..." | "disk:./cache"
        "maxSizeMb": 256,
        "ttlSecs": 300,
        "varyHeaders": ["Accept-Encoding"],
        "skipIfCookie": true,
        "skipPaths": ["/api/auth/**"]
      }
    }
  },

  // ── File Upload ───────────────────────────────────────────────────────────
  "upload": {
    "path": "/upload",
    "dir": "./uploads",
    "maxFileSizeBytes": 10485760,
    "maxTotalSizeBytes": 52428800,
    "maxFiles": 5,
    "allowedMimeTypes": ["image/jpeg", "image/png"]
  },

  // ── Health Check endpoint ─────────────────────────────────────────────────
  // false | true | { "path": "/__health__", ... }
  // Always bypasses auth and rate limiting
  "healthCheck": true,

  // ── Prometheus Metrics ────────────────────────────────────────────────────
  "metrics": {
    "path": "/__metrics__",
    "token": "$METRICS_TOKEN"          // Bearer token (optional)
  },

  // ── Fallback ──────────────────────────────────────────────────────────────
  "fallback": { "status": 200, "file": "./dist/index.html" }
  // Or content-type aware:
  // "fallback": {
  //   "byAccept": {
  //     "html": { "status": 200, "file": "./dist/index.html" },
  //     "json": { "status": 404, "body": { "error": "Not Found" } },
  //     "*":    { "status": 200, "file": "./dist/index.html" }
  //   }
  // }
}
```

### Multi-site (global config)

When you have multiple virtual hosts, use the full form:

```jsonc
{
  "global": {
    "workers": 4,
    "backlog": 1024,
    "shutdownTimeoutSecs": 30,
    "admin": { "bind": "127.0.0.1:2019" }
  },
  "sites": [
    { "host": "app.example.com", "port": 443, "..." },
    { "host": "api.example.com", "port": 443, "..." }
  ]
}
```

### Hot vs cold config fields

`conduit reload` applies these fields **without restart**:

> `headers` · `redirects` · `fallback` · `rateLimit` · `basicAuth` · `apiKey` · `ipFilter` · `limits` · `logging` · `cors` · `securityHeaders` · `responseTime` · `compression` · `proxy.*` (including `cache.*`) · `static` · `staticOptions` · `hotReload` · `upload` · `metrics` · `healthCheck` · `middleware`

These fields require a **restart** (server will tell you which ones changed):

> `port` · `host` · `tls.cert/key/versions/ciphers` · `http2.*` · `global.workers` · `global.backlog` · `global.admin`

---

## Configuration Recipes

### SPA with API (production, Auto-TLS)

```json
{
  "port": 443,
  "tls": { "acme": { "email": "admin@example.com" } },
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

### Frontend development (hot reload)

```json
{
  "port": 3000,
  "logging": "dev",
  "cors": true,
  "hotReload": { "extensions": [".html", ".css", ".js", ".ts"] },
  "static": "./src",
  "staticOptions": { "etag": false, "lastModified": false },
  "proxy": { "/api": "http://localhost:4000" },
  "fallback": { "status": 200, "file": "./src/index.html" }
}
```

### Weighted load balancing

```json
{
  "port": 8080,
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

### Multi-site virtual hosting

```json
[
  {
    "host": "app.example.com",
    "port": 443,
    "tls": { "acme": { "email": "admin@example.com" }, "httpRedirectPort": 80 },
    "static": "./dist",
    "proxy": { "/api": "http://localhost:4000" }
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
```

### TLS termination with HTTP/2

```json
{
  "port": 443,
  "tls": { "cert": "$CERT", "key": "$KEY", "httpRedirectPort": 80 },
  "http2": { "maxConcurrentStreams": 250 },
  "compression": true,
  "securityHeaders": true,
  "proxy": {
    "/api": { "targets": ["http://backend:4000"], "stripPrefix": true }
  },
  "static": "./dist",
  "healthCheck": true
}
```

---

## Docker

### Dockerfile

```dockerfile
FROM scratch
COPY conduit-x86_64-unknown-linux-musl /conduit
COPY conduit.json /conduit.json
ENTRYPOINT ["/conduit", "-c", "/conduit.json"]
```

Or use the official image:

```bash
docker run -p 8080:8080 \
  -v $(pwd)/conduit.json:/conduit.json \
  -v $(pwd)/dist:/dist \
  lopatnov/conduit
```

### docker-compose.yml

```yaml
services:
  proxy:
    image: lopatnov/conduit:latest
    ports:
      - "443:443"
      - "80:80"
    volumes:
      - ./conduit.json:/conduit.json
      - ./dist:/dist
      - ./certs:/certs
    environment:
      - METRICS_TOKEN=secret
    restart: unless-stopped

  api:
    image: my-api:latest
    expose: ["4000"]
```

---

## Benchmarks

See [BENCHMARKS.md](BENCHMARKS.md) for full results.

Quick comparison vs `express-reverse-proxy` running the same workload:

```
Static file (1 KB)
  express-reverse-proxy:  7,842 req/s
  conduit:              156,200 req/s  (+19.9×)

Proxy passthrough
  express-reverse-proxy:  6,103 req/s
  conduit:               84,700 req/s  (+13.9×)

P99 proxy latency
  express-reverse-proxy:  14.8 ms
  conduit:                 1.7 ms  (8.7× lower)
```

Run benchmarks locally:

```bash
cargo bench
```

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

Quick start:

```bash
git clone https://github.com/lopatnov/conduit
cd conduit
cargo build
cargo test
```

---

## Built With

| Crate | Role |
|---|---|
| [pingora](https://github.com/cloudflare/pingora) | Async proxy framework (Cloudflare) |
| [tokio](https://tokio.rs) | Async runtime |
| [axum](https://github.com/tokio-rs/axum) | Admin API + upload server |
| [serde](https://serde.rs) | JSON config parsing |
| [clap](https://clap.rs) | CLI |
| [prometheus](https://github.com/tikv/rust-prometheus) | Metrics |
| [tracing](https://github.com/tokio-rs/tracing) | Structured logging |
| [rcgen](https://github.com/rustls/rcgen) | Self-signed TLS (dev) |
| [instant-acme](https://github.com/InstantDomainSearch/instant-acme) | Let's Encrypt |

---

## License

[Apache 2.0](LICENSE) © [lopatnov](https://github.com/lopatnov)
