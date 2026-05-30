# @lopatnov/conduit

[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dt/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![crates.io](https://img.shields.io/crates/v/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![GitHub stars](https://img.shields.io/github/stars/lopatnov/conduit)](https://github.com/lopatnov/conduit/stargazers)
[![License](https://img.shields.io/github/license/lopatnov/conduit)](https://github.com/lopatnov/conduit/blob/main/LICENSE)
[![GitHub](https://img.shields.io/badge/source-GitHub-181717.svg)](https://github.com/lopatnov/conduit)

> **High-performance reverse proxy and static file server** — one config file, one binary,
> zero runtime dependencies.

Built on [Cloudflare Pingora](https://github.com/cloudflare/pingora) — the same engine that
routes ~1 trillion requests/day at Cloudflare. Distributed as a native Rust binary via npm
for convenience.

---

## Getting Started

**No installation needed:**

```bash
npx @lopatnov/conduit init    # interactive setup wizard
npx @lopatnov/conduit         # start
```

**Install globally** — then just type `conduit`:

```bash
npm install -g @lopatnov/conduit
conduit init
conduit
```

> **How it works:** `postinstall` downloads the correct pre-built native binary for your
> platform from [GitHub Releases](https://github.com/lopatnov/conduit/releases).
> No compilation. Node.js is only needed for the download step — the server itself is a
> standalone Rust binary.

---

## Minimal Config

Create `conduit.json` (or `conduit.yaml`):

```json
{
  "port": 3000,
  "proxy": { "/api": "http://localhost:4000" }
}
```

Run:

```bash
conduit
```

`GET /api/users` → `http://localhost:4000/api/users`. Done.

---

## Common Recipes

### Serve static files

```json
{ "port": 3000, "static": "./dist" }
```

### Reverse proxy to a backend

```json
{ "port": 3000, "proxy": "http://localhost:4000" }
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
      "healthCheck": { "path": "/health", "intervalSecs": 10 },
      "retry": { "attempts": 3, "conditions": ["connection_error", "5xx"] }
    }
  }
}
```

### Production SPA with Auto-TLS (Let's Encrypt)

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
  "rateLimit": { "windowSecs": 60, "limit": 200 },
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

### Multiple sites from one process

```json
[
  {
    "host": "app.example.com",
    "port": 443,
    "tls": { "cert": "$CERT", "key": "$KEY" },
    "static": "./dist",
    "proxy": { "/api": "http://api:4000" }
  },
  {
    "host": "admin.example.com",
    "port": 443,
    "tls": { "cert": "$CERT", "key": "$KEY" },
    "basicAuth": { "users": { "admin": "$ADMIN_PASS" }, "challenge": true },
    "static": "./admin-ui"
  }
]
```

---

## CLI Reference

```text
conduit                     start server (reads conduit.json / conduit.yaml)
conduit -c <file>           use a specific config file (.json or .yaml)
conduit --version           print version

conduit init                interactive setup wizard
conduit validate            validate config (exit 0 = OK)
conduit probe               HEAD each upstream, show latency
conduit fmt [--write]       pretty-print config to stdout or file

conduit reload              hot-reload config without restart
conduit status              show uptime and inflight requests
conduit upstreams           list upstream health and latency
conduit upstreams add  --route PATH --target URL [--weight N] [--site LABEL]
conduit upstreams remove --route PATH --target URL [--site LABEL]
conduit upstreams weight --route PATH --target URL --weight N [--site LABEL]
conduit shutdown            graceful shutdown
```

---

## Features

| Feature | Details |
| --- | --- |
| **Static files** | ETag, Last-Modified, Range, dotfile control, pre-compressed `.br`/`.gz` |
| **Compression** | gzip + brotli (async, streaming) |
| **Reverse proxy** | 7 load-balancing strategies; upstream health checks; retry on failure |
| **Proxy cache** | Memory, Redis, or disk store; Vary headers; skip-paths |
| **Auto-TLS** | Let's Encrypt via ACME — automatic issue and renewal |
| **HTTP/2** | ALPN negotiation; H/2 upstream support |
| **WebSocket** | Transparent proxying |
| **Hot config reload** | `conduit reload` — zero-downtime, no restart |
| **IP filtering** | CIDR allow/deny lists; trust X-Forwarded-For |
| **Rate limiting** | Token-bucket, keyed by IP or header; Redis-backed for clusters |
| **Auth** | Basic auth + API key, per-route skip-paths |
| **CORS** | Origin allow-list, credentials, preflight |
| **Security headers** | CSP, HSTS, X-Frame-Options, Referrer-Policy |
| **Health check** | `/__health__` with optional upstream status |
| **Prometheus** | `/__metrics__` — request counters, duration histograms, cache metrics |
| **File upload** | `multipart/form-data` — UUID filenames, MIME validation |
| **Redirects** | Named params (`:slug`), 301/302/307/308 |
| **Advanced routing** | Glob path + method + header + query predicates |
| **Virtual hosting** | Multiple sites (`host` matching) in one process |
| **SPA fallback** | Per-Accept-type fallback rules |
| **Structured logging** | `dev`, `combined`, `json`, `short`, `common` formats |
| **Rhai scripting** | Inline middleware scripts for custom logic |
| **YAML config** | `conduit.yaml` / `conduit.yml` as alternative to JSON |

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

Unsupported platform? Install from source:

```bash
cargo install lopatnov-conduit
```

---

## Links

- 📦 [npm package](https://www.npmjs.com/package/@lopatnov/conduit)
- 🦀 [crates.io package](https://crates.io/crates/lopatnov-conduit)
- 🐳 [Docker image](https://github.com/lopatnov/conduit/pkgs/container/conduit) (`ghcr.io/lopatnov/conduit`)
- 📖 [Full documentation](https://github.com/lopatnov/conduit)
- ⚙️ [Configuration reference](https://github.com/lopatnov/conduit/blob/main/docs/configuration.md)
- 🚀 [Deployment guide](https://github.com/lopatnov/conduit/blob/main/docs/deployment.md)
- 📊 [Benchmarks](https://github.com/lopatnov/conduit/blob/main/docs/benchmarks.md)
- 🐛 [Report a bug](https://github.com/lopatnov/conduit/issues)
- 💬 [Discussions](https://github.com/lopatnov/conduit/discussions)

---

## Contributing

Contributions are welcome! Read [CONTRIBUTING.md](https://github.com/lopatnov/conduit/blob/main/CONTRIBUTING.md)
before opening a pull request.

Bug reports → [GitHub Issues](https://github.com/lopatnov/conduit/issues).
Security vulnerabilities → [GitHub Security Advisories](https://github.com/lopatnov/conduit/security/advisories).
Found it useful? A ⭐ on GitHub helps others discover the project.

---

## License

[Apache 2.0](https://github.com/lopatnov/conduit/blob/main/LICENSE) ©
2024–2026 [Oleksandr Lopatnov](https://github.com/lopatnov)
