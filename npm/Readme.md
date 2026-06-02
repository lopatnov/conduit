# @lopatnov/conduit

[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dt/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![GitHub](https://img.shields.io/badge/source-GitHub-181717.svg)](https://github.com/lopatnov/conduit)
[![crates.io downloads](https://img.shields.io/crates/d/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![License](https://img.shields.io/github/license/lopatnov/conduit)](https://github.com/lopatnov/conduit/blob/main/LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/lopatnov/conduit)](https://github.com/lopatnov/conduit/stargazers)

> **Production-grade reverse proxy and API gateway** — TLS, rate limiting,
> JWT auth, load balancing, caching, Prometheus metrics. One config file,
> one binary, zero runtime dependencies.

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
> No compilation. Node.js is only needed for the download — the server itself is a
> standalone Rust binary with no Node.js dependency at runtime.

---

## Standard vs Full binary

The npm package installs the **standard** binary. It covers the majority of
production use cases: TLS, reverse proxying, static files, rate limiting,
basic/API-key auth, compression, hot-reload, health checks, and Prometheus metrics.

Features that require optional compile-time flags are **not included** in the
standard npm binary:

| Feature                    | Requires               | How to get it                                                          |
| -------------------------- | ---------------------- | ---------------------------------------------------------------------- |
| JWT Bearer-token auth      | `--features jwt`       | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |
| Auto-TLS (Let's Encrypt)   | `--features acme`      | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |
| Response caching           | `--features cache`     | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |
| Redis rate limiting        | `--features redis`     | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |
| WASM plugin middleware     | `--features wasm`      | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |
| Rhai scripting middleware  | `--features rhai`      | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |
| OpenTelemetry OTLP tracing | `--features otlp`      | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |
| TCP proxy mode             | `--features tcp`       | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |
| Consumer model             | `--features consumers` | [Download full binary ↗](https://github.com/lopatnov/conduit/releases) |

**To get all features** download `conduit-*-full.tar.gz` from
[GitHub Releases](https://github.com/lopatnov/conduit/releases), or
build from source with `cargo install lopatnov-conduit --features full`.

---

## Minimal Config

Create `conduit.yaml` (or `conduit.json`):

```yaml
port: 3000
proxy:
  /api: "http://localhost:4000"
```

Run:

```bash
conduit
```

`GET /api/users` → `http://localhost:4000/api/users`. Done.

---

## Common Recipes

### Serve static files

```yaml
port: 3000
static: ./dist
```

### Reverse proxy to a backend

```yaml
port: 3000
proxy: "http://localhost:4000"
```

### SPA + API (most common)

```yaml
port: 3000
static: ./dist
proxy:
  /api: "http://localhost:4000"
fallback:
  status: 200
  file: ./dist/index.html
```

### Dev server with hot reload

```yaml
port: 3000
logging: dev
hotReload: true
cors: true
static: ./src
proxy:
  /api: "http://localhost:4000"
fallback:
  status: 200
  file: ./src/index.html
```

### Load-balanced backend with health checks

```yaml
port: 8080
proxy:
  /api:
    targets:
      - "http://api1:4000"
      - "http://api2:4000"
      - "http://api3:4000"
    strategy: least-conn
    healthCheck:
      path: /health
      intervalSecs: 10
    retry:
      attempts: 3
      conditions: [connection_error, "5xx"]
```

### Production HTTPS with manual certificates

```yaml
port: 443
tls:
  cert: /etc/tls/fullchain.pem
  key: /etc/tls/privkey.pem
  httpRedirectPort: 80
http2: true
securityHeaders: true
compression: true
static: ./dist
proxy:
  /api:
    targets: ["http://api1:4000", "http://api2:4000"]
    strategy: least-conn
    stripPrefix: true
rateLimit:
  windowSecs: 60
  limit: 200
healthCheck: true
metrics:
  path: /__metrics__
  token: "$METRICS_TOKEN"
```

> **Auto-TLS** (`tls.acme`) and **response caching** (`proxy.*.cache`) require
> the full binary — see [Standard vs Full binary](#standard-vs-full-binary).

### Multiple sites from one process

```yaml
global:
  admin:
    bind: "127.0.0.1:2019"

sites:
  - host: app.example.com
    port: 443
    tls:
      cert: "$CERT_PATH"
      key: "$KEY_PATH"
    static: ./dist
    proxy:
      /api: "http://api:4000"

  - host: admin.example.com
    port: 443
    tls:
      cert: "$CERT_PATH"
      key: "$KEY_PATH"
    basicAuth:
      users: { admin: "$ADMIN_PASS" }
      challenge: true
    static: ./admin-ui
```

---

## CLI Reference

```text
conduit                       start server (reads conduit.yaml / conduit.json)
conduit -c <file>             use a specific config file (.yaml or .json)
conduit --version             print version
conduit --help                show all options

conduit init [--yes]          interactive setup wizard (--yes = non-interactive)
conduit validate              validate config (exit 0 = OK, exit 1 = errors)
conduit probe                 HEAD each upstream, show latency table
conduit fmt [--write]         pretty-print / normalise config

conduit reload   [--admin ADDR]    hot-reload config without restart
conduit status   [--admin ADDR]    show uptime and in-flight requests
conduit status   [--admin ADDR] --upstream   show upstream health table
conduit upstreams [--admin ADDR]   list upstream health and latency
conduit upstreams add    --route PATH --target URL [--weight N] [--site LABEL]
conduit upstreams remove --route PATH --target URL [--site LABEL]
conduit upstreams weight --route PATH --target URL --weight N [--site LABEL]
conduit shutdown [--admin ADDR]    graceful shutdown

conduit completions bash|zsh|fish|power-shell|elvish
conduit man                   generate man page (roff)
```

Admin commands connect to `127.0.0.1:2019` by default. Override with
`--admin ADDR` or `CONDUIT_ADMIN` environment variable.

---

## Features

| Feature                    | Details                                                                                 |
| -------------------------- | --------------------------------------------------------------------------------------- |
| **Reverse proxy**          | 8 load-balancing strategies; health checks; retry; failover; traffic mirroring          |
| **Static files**           | ETag, Last-Modified, Range requests, pre-compressed `.br`/`.gz` sidecars                |
| **TLS**                    | Manual certificates, HTTP→HTTPS redirect, mTLS client certificates                      |
| **Auto-TLS** ¹             | Let's Encrypt via ACME — automatic issue and renewal                                    |
| **HTTP/2**                 | ALPN negotiation, h2c (cleartext), upstream H/2 support                                 |
| **Compression**            | gzip + Brotli + Zstd (async, streaming, configurable Content-Type filter)               |
| **WebSocket**              | Transparent `Connection: Upgrade` proxying                                              |
| **Proxy cache** ¹          | Memory, Redis, or disk store; stale-while-revalidate; thundering-herd lock              |
| **IP filtering**           | CIDR allow/deny lists; trust `X-Forwarded-For`; runtime deny-list via Admin API         |
| **Rate limiting**          | Token-bucket, per-IP or per-header; burst capacity; Redis-backed for clusters ¹         |
| **Auth**                   | Basic Auth, API key; JWT ¹ (HS256/RS256/ES256 + JWKS); Forward Auth ¹; Consumer model ¹ |
| **CORS**                   | Origin allow-list, credentials mode, preflight                                          |
| **Security headers**       | HSTS, CSP, X-Frame-Options, Permissions-Policy, Referrer-Policy, allowedHosts           |
| **Request transforms**     | Set/remove headers before upstream; inject JWT claims (`{{ jwt.sub }}`)                 |
| **Response transforms**    | Set/remove headers on upstream response                                                 |
| **Scripting middleware** ¹ | Rhai scripts or WASM plugins — request and response phase                               |
| **Reliability**            | Circuit breaker, outlier detection, retry budget, priority load-shedding                |
| **Hot reload**             | `conduit reload` — zero-downtime, no dropped connections                                |
| **Health check**           | `/__health__` with optional upstream status, latency, ejection state                    |
| **Prometheus**             | `/__metrics__` — 11 metrics including per-upstream counters and latency histograms      |
| **OpenTelemetry** ¹        | OTLP distributed tracing to Grafana Tempo / Jaeger                                      |
| **File upload** ¹          | `multipart/form-data` — UUID filenames, MIME allowlist, size limits                     |
| **TCP proxy** ¹            | Raw TCP passthrough — MySQL, PostgreSQL, Redis, SMTP                                    |
| **Redirects**              | Named params (`:slug`), 301/302/307/308                                                 |
| **Advanced routing**       | Glob path + method + header regex + query + cookie predicates                           |
| **Virtual hosting**        | Multiple sites (`host` matching) from one process                                       |
| **SPA fallback**           | Per-`Accept`-type fallback rules                                                        |
| **Structured logging**     | `dev`, `combined`, `json`, `short`, `common` formats                                    |
| **YAML config**            | `conduit.yaml` / `conduit.yml` — YAML recommended; JSON also supported                  |
| **Kubernetes** ¹           | `ConduitSite` CRD config provider                                                       |

> ¹ Not included in the standard npm binary — requires the [full binary](#standard-vs-full-binary).

---

## Supported Platforms

| Platform | Architecture           | Standard | Full |
| -------- | ---------------------- | :------: | :--: |
| Linux    | x86-64 (glibc)         |    ✅    |  ✅  |
| Linux    | x86-64 (musl / Docker) |    ✅    |  ✅  |
| Linux    | ARM64                  |    ✅    |  ✅  |
| Linux    | RISC-V 64              |    ✅    |  —   |
| macOS    | Intel (x86-64)         |    ✅    |  ✅  |
| macOS    | Apple Silicon (ARM64)  |    ✅    |  ✅  |
| Windows  | x86-64                 |    ✅    |  ✅  |

Unsupported platform? Build from source:

```bash
cargo install lopatnov-conduit            # standard
cargo install lopatnov-conduit --features full   # all features
```

---

## Links

- 📦 [npm package](https://www.npmjs.com/package/@lopatnov/conduit)
- 🦀 [crates.io package](https://crates.io/crates/lopatnov-conduit)
- 🐳 [Docker image](https://github.com/lopatnov/conduit/pkgs/container/conduit) (`ghcr.io/lopatnov/conduit`)
- 📖 [Full documentation](https://github.com/lopatnov/conduit/tree/main/docs)
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
