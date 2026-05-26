# @lopatnov/conduit

[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dm/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/lopatnov/conduit/blob/main/LICENSE)
[![GitHub](https://img.shields.io/badge/source-GitHub-181717.svg)](https://github.com/lopatnov/conduit)

> **High-performance reverse proxy and static file server** — one JSON config, one binary, zero runtime dependencies.

Built on [Cloudflare Pingora](https://github.com/cloudflare/pingora). Distributed as a native Rust binary via npm for convenience.

---

## Getting Started

**No installation needed:**

```bash
npx @lopatnov/conduit init    # interactive setup wizard
npx @lopatnov/conduit         # start
```

**Or install globally** — then just type `conduit`:

```bash
npm install -g @lopatnov/conduit
```

> **How it works:** `postinstall` downloads the correct pre-built native binary for your platform from
> [GitHub Releases](https://github.com/lopatnov/conduit/releases). No compilation. Node.js is only
> needed for `npx` / `npm install` — the server runs as a standalone native binary.

---

## Minimal Config

Create `conduit.json`:

```json
{
  "port": 3000,
  "proxy": { "/api": "http://localhost:4000" }
}
```

Run it:

```bash
conduit
```

Done. `GET /api/users` → `http://localhost:4000/api/users`.

---

## Common Recipes

### Serve static files

```json
{
  "port": 3000,
  "static": "./dist"
}
```

### Reverse proxy to a backend

```json
{
  "port": 3000,
  "proxy": "http://localhost:4000"
}
```

### SPA + API (most common)

```json
{
  "port": 3000,
  "static": "./dist",
  "proxy": { "/api": "http://localhost:4000" },
  "fallback": { "status": 200, "file": "./dist/index.html" }
}
```

### Dev server with hot reload

```json
{
  "port": 3000,
  "logging": "dev",
  "hotReload": true,
  "cors": true,
  "static": "./src",
  "proxy": { "/api": "http://localhost:4000" },
  "fallback": { "status": 200, "file": "./src/index.html" }
}
```

### Load-balanced backend with health checks

```json
{
  "port": 8080,
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000", "http://api3:4000"],
      "strategy": "least-conn",
      "healthCheck": { "path": "/health", "intervalSecs": 10 }
    }
  }
}
```

### Production SPA with Auto-TLS

```json
{
  "port": 443,
  "tls": { "acme": { "email": "admin@example.com" } },
  "compression": true,
  "securityHeaders": true,
  "static": "./dist",
  "staticOptions": { "maxAge": "7d", "preCompressed": true },
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "retry": { "attempts": 3, "conditions": ["connection_error", "5xx"] },
      "cache": { "store": "memory", "ttlSecs": 60, "skipIfCookie": true }
    }
  },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__", "token": "$METRICS_TOKEN" },
  "rateLimit": { "windowSecs": 60, "limit": 200 },
  "fallback": {
    "byAccept": {
      "html": { "status": 200, "file": "./dist/index.html" },
      "json": { "status": 404, "body": { "error": "Not Found" } }
    }
  }
}
```

---

## CLI Reference

```
conduit                     start server (reads conduit.json)
conduit -c <file>           use a specific config file
conduit --version           print version

conduit init                interactive setup wizard
conduit validate            validate config (exit 0 = OK)
conduit probe               HEAD each upstream, show latency
conduit fmt [--write]       pretty-print config to stdout or file

conduit reload              hot-reload config without restart
conduit status              show uptime and inflight requests
conduit upstreams           list upstream health and latency
conduit shutdown            graceful shutdown
```

---

## Features

| Feature | Details |
| --- | --- |
| **Static files** | ETag, Last-Modified, Range, dotfile control |
| **Compression** | gzip + brotli (async, streaming), pre-compressed `.br`/`.gz` |
| **Reverse proxy** | Round-robin, weighted, random, least-conn, IP-hash, consistent-hash |
| **Load balancing** | 7 strategies; upstream health checks; retry on failure |
| **Auto-TLS** | Let's Encrypt via ACME — automatic issue and renewal |
| **HTTP/2** | ALPN negotiation; H/2 upstream support |
| **WebSocket** | Transparent proxying |
| **Hot config reload** | `conduit reload` — zero-downtime, no restart |
| **IP filtering** | CIDR allow/deny lists; trust X-Forwarded-For |
| **Rate limiting** | Token-bucket, keyed by IP or header |
| **Auth** | Basic auth + API key, per-route skip-paths |
| **CORS** | Origin allow-list, credentials, preflight |
| **Security headers** | CSP, HSTS, X-Frame-Options, Referrer-Policy |
| **Proxy cache** | In-memory cache with TTL, Vary, skip-paths |
| **Health check** | `/__health__` with optional upstream status |
| **Prometheus** | `/__metrics__` — request counters, duration histograms |
| **File upload** | `multipart/form-data` — UUID filenames, MIME validation |
| **Redirects** | Named params (`:slug`), 301/302/307/308 |
| **Routes** | Glob path + method + header + query predicates |
| **Virtual hosting** | Multiple sites (`host` matching) in one process |
| **SPA fallback** | Per-Accept-type fallback rules |
| **Structured logging** | `dev`, `combined`, `json`, `short`, `common` formats |

---

## Supported Platforms

| Platform | Architecture |
| --- | --- |
| Linux | x86-64 (glibc) |
| Linux | x86-64 (musl / Docker) |
| Linux | ARM64 |
| macOS | Intel (x86-64) |
| macOS | Apple Silicon (ARM64) |
| Windows | x86-64 |

If your platform isn't listed or the download fails, install from source:

```bash
cargo install lopatnov-conduit
```

---

## Links

- 📦 [npm package](https://www.npmjs.com/package/@lopatnov/conduit)
- 🦀 [crates.io package](https://crates.io/crates/lopatnov-conduit)
- 📖 [Full documentation & source](https://github.com/lopatnov/conduit)
- 🐛 [Report a bug](https://github.com/lopatnov/conduit/issues)

---

## License

[Apache 2.0](https://github.com/lopatnov/conduit/blob/main/LICENSE) ©
[Oleksandr Lopatnov](https://github.com/lopatnov)
