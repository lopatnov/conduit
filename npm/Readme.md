# @lopatnov/conduit

[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dm/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/lopatnov/conduit/blob/main/LICENSE)
[![GitHub](https://img.shields.io/badge/source-GitHub-181717.svg)](https://github.com/lopatnov/conduit)

> **High-performance reverse proxy and static file server** — one JSON file, one binary, zero dependencies.

Built on [Cloudflare Pingora](https://github.com/cloudflare/pingora), distributed as a native Rust binary via npm for convenience.

---

## Installation

```bash
# Try without installing (always latest)
npx @lopatnov/conduit

# Install globally — then just type `conduit`
npm install -g @lopatnov/conduit
```

> **How it works:** The `postinstall` script downloads the correct pre-built native binary for your
> platform from [GitHub Releases](https://github.com/lopatnov/conduit/releases).
> No compilation required. The Node.js runtime is only needed for the `npx` / `npm install` step —
> the server itself runs as a standalone native binary.

To skip the automatic download (e.g. you built from source):

```bash
CONDUIT_SKIP_DOWNLOAD=1 npm install -g @lopatnov/conduit
```

---

## Quick Start

```bash
conduit init        # interactive setup wizard
conduit             # start the server
conduit validate    # validate config without starting
```

Minimal `conduit.json` — serve static files and proxy an API:

```json
{
  "port": 3000,
  "static": "./dist",
  "proxy": { "/api": "http://localhost:4000" }
}
```

```
GET /           → ./dist/index.html
GET /style.css  → ./dist/style.css
GET /api/users  → http://localhost:4000/api/users
```

---

## Features

| Feature | Description |
| --- | --- |
| **Static files** | ETag, Last-Modified, Range, gzip/brotli compression |
| **Reverse proxy** | Round-robin, weighted, least-conn, IP-hash, consistent-hash |
| **TLS** | Manual certs or Auto-TLS via Let's Encrypt (ACME) |
| **HTTP/2** | ALPN negotiation, configurable streams |
| **IP filtering** | CIDR allow/deny lists, trust X-Forwarded-For |
| **Rate limiting** | Token-bucket, keyed by IP or header |
| **Basic auth** | Per-site, with configurable skip-paths |
| **CORS** | Preflight handling, origin allow-list |
| **Security headers** | CSP, HSTS, X-Frame-Options, and more |
| **Health check** | `/__health__` with upstream status |
| **Prometheus metrics** | `/__metrics__` with request counters and histograms |
| **Redirects** | Named params (`:slug`), status codes 301/302/307/308 |
| **SPA fallback** | Serve `index.html` for unknown paths |
| **Hot config reload** | `conduit reload` — no restart needed |
| **Virtual hosting** | Multiple sites, one process |

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

conduit reload              hot-reload config
conduit status              server uptime and inflight requests
conduit upstreams           upstream health and latency
conduit shutdown            graceful shutdown
```

---

## Configuration Example

```json
{
  "port": 443,
  "tls": {
    "acme": { "email": "admin@example.com" }
  },
  "http2": true,
  "compression": true,
  "securityHeaders": true,
  "cors": { "origins": ["https://app.example.com"], "credentials": true },
  "rateLimit": { "windowSecs": 60, "limit": 200 },
  "static": "./dist",
  "staticOptions": { "maxAge": "7d" },
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "retry": { "attempts": 3, "conditions": ["connection_error", "5xx"] }
    }
  },
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

See the [full documentation](https://github.com/lopatnov/conduit#configuration) for all options.

---

## Supported Platforms

| Platform | Architecture          |
| -------- | --------------------- |
| Linux    | x86-64 (glibc)        |
| Linux    | x86-64 (musl/Docker)  |
| Linux    | ARM64                 |
| macOS    | Intel (x86-64)        |
| macOS    | Apple Silicon (ARM64) |
| Windows  | x86-64                |

If your platform is not listed or the download fails, install from source:

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
