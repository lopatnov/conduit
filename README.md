# Conduit

[![CI](https://github.com/lopatnov/conduit/actions/workflows/ci.yml/badge.svg)](https://github.com/lopatnov/conduit/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![crates.io downloads](https://img.shields.io/crates/d/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dt/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![License](https://img.shields.io/badge/license-Apache_2.0-blue.svg)](LICENSE)

**Production-grade reverse proxy and API gateway** built on [Cloudflare Pingora](https://github.com/cloudflare/pingora).

A **single binary, single config file** that replaces nginx + Traefik + a rate-limiter + a JWT validator + a file server — with a 14 MB footprint, ~28 ms cold start, and zero runtime dependencies.

```bash
npx @lopatnov/conduit init   # generate config interactively
npx @lopatnov/conduit        # start
```

---

## Table of Contents

- [Quick Start](#quick-start)
- [What Conduit does](#what-conduit-does)
- [Installation](#installation)
- [Building from Source](docs/building.md) ↗
- [CLI Commands](#cli-commands)
- [Configuration](#configuration)
- [Recipes](#recipes)
- [Admin API](#admin-api)
- [Docker](#docker)
- [Editor Integration](#editor-integration-json-schema)
- [Benchmarks](docs/benchmarks.md) ↗
- [Contributing](#contributing)
- [License](#license)

---

## Quick Start

```bash
# 1. Generate a config
conduit init          # interactive wizard
conduit init --yes    # non-interactive, accept defaults → conduit.yaml

# 2. Validate before starting
conduit validate

# 3. Start
conduit
# Listening on http://0.0.0.0:8080
```

Minimal hand-written config — YAML and JSON are equivalent:

```yaml
# conduit.yaml
port: 8080
static: ./dist
proxy:
  /api: http://localhost:4000
```

```
GET /          → ./dist/index.html
GET /logo.png  → ./dist/logo.png
GET /api/users → http://localhost:4000/api/users
```

Verify it's running:

```bash
curl http://localhost:8080/__health__
# {"status":"ok"}

curl http://localhost:8080/api/users
# proxied to http://localhost:4000/api/users
```

**→ Full examples:** [`examples/`](examples/) · **→ Configuration reference:** [docs/configuration.md](docs/configuration.md)

---

## What Conduit does

<table>
<tr><td><b>Proxying</b></td><td>Reverse proxy with 8 load-balancing strategies, health checks, outlier detection, circuit breaker, sticky sessions, failover</td></tr>
<tr><td><b>Static files</b></td><td>ETag, Range, pre-compressed <code>.br</code>/<code>.gz</code>, <code>Cache-Control</code>, SPA fallback</td></tr>
<tr><td><b>TLS</b></td><td>Manual certificates, auto-TLS via Let's Encrypt (ACME), mTLS client certificates, HTTP/2</td></tr>
<tr><td><b>Auth</b></td><td>Basic Auth, API key, JWT (HS256/RS256/ES256 + JWKS), Forward Auth, Consumer model</td></tr>
<tr><td><b>Rate limiting</b></td><td>Token-bucket per IP or header, burst capacity, Redis for multi-instance, per-route limits</td></tr>
<tr><td><b>Caching</b></td><td>In-memory, Redis, disk — stale-while-revalidate, stale-if-error, thundering-herd lock</td></tr>
<tr><td><b>Reliability</b></td><td>Retry with budget + jitter, per-try timeout, body buffering for replay, priority load-shedding, traffic mirroring</td></tr>
<tr><td><b>Routing</b></td><td>Path glob, method, header regex, cookie, query params — ordered route table</td></tr>
<tr><td><b>Middleware</b></td><td>Rhai scripting, WebAssembly plugins (Wasmtime), request/response header transforms, CORS, compression, fault injection</td></tr>
<tr><td><b>Observability</b></td><td>Prometheus metrics (11 metrics), OpenTelemetry OTLP tracing, structured JSON access log, <code>X-Request-ID</code></td></tr>
<tr><td><b>Operations</b></td><td>Hot config reload (zero dropped connections), Admin API, IP deny-list, TCP proxy mode, file upload</td></tr>
<tr><td><b>Deployment</b></td><td>Single binary, Docker <code>FROM scratch</code>, Kubernetes CRD, systemd, YAML/JSON config, env var secrets</td></tr>
</table>

### When to choose Conduit

| | Conduit | nginx | Traefik | Caddy |
|---|---|---|---|---|
| Config format | YAML / JSON file | Custom DSL | YAML / TOML / CLI | Caddyfile / JSON |
| JWT validation | ✅ built-in | ⚠️ plugin (Lua) | ⚠️ forward-auth only | ⚠️ plugin |
| Rate limiting | ✅ built-in | ⚠️ module | ✅ built-in | ⚠️ plugin |
| Scripting middleware | ✅ Rhai + WASM | ✅ Lua (OpenResty) | ❌ | ❌ |
| Binary size | **14 MB** | ~1 MB | ~110 MB | ~50 MB |
| Memory (idle) | **~8 MB** | ~5 MB | ~28 MB | ~15 MB |
| Hot reload | ✅ zero-drop | ⚠️ reload signal | ✅ | ✅ |
| Auto-TLS | ✅ ACME built-in | ❌ (certbot) | ✅ | ✅ |
| WASM plugins | ✅ | ❌ | ❌ | ❌ |

> Conduit is a good fit when you want **a single tool** that handles proxying,
> auth, rate limiting, and basic scripting without separate sidecars or plugins.
> For very high traffic (>100 k req/s static files) or complex Layer 4 routing,
> nginx or Envoy scale better.

---

## Installation

> **Standard vs full:** the standard binary covers the vast majority of use cases.
> Add `--features` when you need JWT auth, scripting (Rhai/WASM), OTLP tracing,
> auto-TLS (ACME), Redis, TCP proxy, file upload, or Kubernetes CRD mode.
> See the [full features table](#optional-features).

### npx — no installation needed

```bash
npx @lopatnov/conduit init      # generate config
npx @lopatnov/conduit           # start server
npx @lopatnov/conduit validate  # validate config
```

Downloads and caches the platform binary on first run. Always **standard** build.

### npm global

```bash
npm install -g @lopatnov/conduit
conduit validate
conduit
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/lopatnov/conduit/releases):

| Platform             | Standard                                    | Full (all 14 features)                           |
| -------------------- | ------------------------------------------- | ------------------------------------------------ |
| Linux x86-64         | `conduit-x86_64-unknown-linux-gnu.tar.gz`   | `conduit-x86_64-unknown-linux-gnu-full.tar.gz`   |
| Linux x86-64 musl    | `conduit-x86_64-unknown-linux-musl.tar.gz`  | `conduit-x86_64-unknown-linux-musl-full.tar.gz`  |
| Linux ARM64          | `conduit-aarch64-unknown-linux-gnu.tar.gz`  | `conduit-aarch64-unknown-linux-gnu-full.tar.gz`  |
| Linux RISC-V 64      | `conduit-riscv64gc-unknown-linux-gnu.tar.gz`| —                                                |
| macOS Intel          | `conduit-x86_64-apple-darwin.tar.gz`        | `conduit-x86_64-apple-darwin-full.tar.gz`        |
| macOS Apple Silicon  | `conduit-aarch64-apple-darwin.tar.gz`       | `conduit-aarch64-apple-darwin-full.tar.gz`       |
| Windows x86-64       | `conduit-x86_64-pc-windows-msvc.exe.zip`    | `conduit-x86_64-pc-windows-msvc-full.exe.zip`    |

```bash
# Linux x86-64 — standard
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz && ./conduit --version

# Linux x86-64 — full
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu-full.tar.gz \
  | tar xz && ./conduit --version
```

### cargo install

```bash
cargo install lopatnov-conduit            # standard
cargo install lopatnov-conduit --features full   # all features
```

### Optional features

| Feature         | What it enables                                                            |
| --------------- | -------------------------------------------------------------------------- |
| `jwt`           | JWT Bearer-token auth + JWKS URL (`jwtAuth`)                               |
| `consumers`     | Named API clients with per-consumer credentials and rate limits            |
| `forward-auth`  | Delegate auth to an external HTTP service (`forwardAuth`)                  |
| `rhai`          | Rhai scripting middleware (`type: "script"`)                               |
| `wasm`          | WebAssembly plugin middleware (`type: "wasm"`) via Wasmtime                |
| `tcp`           | Raw TCP proxy mode (`type: "tcp"` site)                                    |
| `upload`        | Multipart file upload handler (`upload:` site config)                      |
| `redis`         | Redis-backed rate limiting and caching                                     |
| `cache`         | Response caching (`proxy.*.cache`)                                         |
| `disk-cache`    | Disk-backed cache store (`cache.store: "disk:/path"`)                      |
| `acme`          | Auto-TLS via Let's Encrypt (`tls.acme`)                                    |
| `fault-injection` | Fault injection for chaos testing                                        |
| `otlp`          | OpenTelemetry OTLP distributed tracing (`global.otlp`)                     |
| `kubernetes`    | Kubernetes CRD config provider (`--kubernetes-namespace`)                  |
| `full`          | All of the above                                                           |

For build instructions, cross-compilation, and troubleshooting see **[docs/building.md](docs/building.md)**.

---

## CLI Commands

```text
conduit [-c FILE]                       start server (default config: conduit.yaml/json)
conduit validate [-c FILE]              validate config — exit 0 = ok, exit 1 = errors
conduit fmt [-c FILE] [--write]         pretty-print / normalise config in place
conduit init [--yes] [-o FILE]          interactive setup wizard (--yes = non-interactive)
conduit probe [-c FILE]                 ping all configured upstreams, show latency

conduit reload   [--admin ADDR]         hot-reload config (zero dropped connections)
conduit status   [--admin ADDR]         version, uptime, in-flight requests
conduit status   [--admin ADDR] --upstream   upstream health table (latency, ejected, 5xx)
conduit shutdown [--admin ADDR]         graceful shutdown
conduit upstreams [--admin ADDR]        list upstream health + latency
conduit upstreams add    --route PATH --target URL [--weight N]
conduit upstreams remove --route PATH --target URL
conduit upstreams weight --route PATH --target URL --weight N

conduit completions bash|zsh|fish|power-shell|elvish   shell completion script
conduit man                             generate man page (roff)
```

Full flag reference and exit codes: **[docs/cli.md](docs/cli.md)**

---

## Configuration

Config is a single YAML or JSON file. Conduit reads `conduit.yaml` (or `conduit.json`)
by default; pass `-c path/to/file` to use another.

All string values support environment variable substitution: `"$VAR"` is replaced at
startup. Never hard-code secrets — use env vars or a secrets manager.

```yaml
# conduit.yaml — annotated overview of common fields
port: 443
host: example.com          # virtual host (omit for catch-all)

tls:
  acme:                    # auto-TLS via Let's Encrypt (--features acme)
    email: admin@example.com

http2: true
compression: true
securityHeaders: true

proxy:
  /api:
    targets:
      - http://api-1:4000
      - http://api-2:4000
    strategy: least-conn
    healthCheck: { path: /health }
    retry: { attempts: 2, conditions: [5xx, connection_error] }
    cache: { store: memory, ttlSecs: 60 }

rateLimit:
  windowSecs: 60
  limit: 300

jwtAuth:                   # --features jwt
  jwksUrl: https://auth.example.com/.well-known/jwks.json
  audience: [my-api]

logging: json
metrics: { path: /__metrics__ }
healthCheck: true
```

**→ All fields with examples:** [docs/configuration.md](docs/configuration.md)  
**→ Ready-to-run configs:** [examples/](examples/)

---

## Recipes

### Local dev server

```yaml
port: 3000
logging: dev
cors: true
hotReload: true
static: ./src
proxy:
  /api: http://localhost:4000
fallback: { file: ./src/index.html, status: 200 }
```

### JWT API gateway

```yaml
port: 8080
jwtAuth:                              # --features jwt
  jwksUrl: https://auth.example.com/.well-known/jwks.json
requestTransform:
  setHeaders:
    X-User-ID: "{{ jwt.sub }}"        # inject claim into upstream request
proxy:
  /users: http://users-svc:4001
  /orders: http://orders-svc:4002
rateLimit: { windowSecs: 60, limit: 500 }
maskErrors: true
metrics: { path: /__metrics__ }
```

### Multi-site (one process, multiple domains)

```yaml
sites:
  - host: app.example.com
    port: 443
    tls: { acme: { email: admin@example.com } }
    static: ./dist
    proxy: { /api: http://api:4000 }

  - host: admin.example.com
    port: 443
    tls: { acme: { email: admin@example.com } }
    basicAuth: { users: { admin: "$ADMIN_PASSWORD" } }
    proxy: http://admin-backend:5000
```

**→ 30+ more scenarios:** [docs/recipes.md](docs/recipes.md) — HTTPS, load balancing,
failover, circuit breaker, caching, security hardening, observability, Kubernetes.

---

## Admin API

Optional local management server. Runs on loopback only — never exposed publicly.

```yaml
global:
  admin:
    bind: "127.0.0.1:2019"
    token: "$ADMIN_TOKEN"    # optional Bearer token
```

```bash
conduit reload                                     # hot-reload config
conduit status                                     # uptime, version, in-flight count
conduit status --upstream                          # upstream health table
conduit upstreams add --route /api --target http://new-backend:4000

# Admin API directly
curl http://localhost:2019/status
curl -X POST http://localhost:2019/reload
curl -X DELETE "http://localhost:2019/cache/purge?url=https://example.com/api/data"
curl -X POST http://localhost:2019/ip-deny -d '{"cidr":"1.2.3.0/24"}'
```

**→ All endpoints with request/response examples:** [docs/admin.md](docs/admin.md)

---

## Docker

```bash
# Standard (~14 MB musl, FROM scratch)
docker pull ghcr.io/lopatnov/conduit:latest

# Full — JWT, Rhai, WASM, OTLP, ACME, Redis, TCP proxy, Kubernetes, etc. (~29 MB)
docker pull ghcr.io/lopatnov/conduit:latest-full

docker run -p 8080:8080 \
  -v $(pwd)/conduit.yaml:/etc/conduit/conduit.yaml:ro \
  ghcr.io/lopatnov/conduit:latest -c /etc/conduit/conduit.yaml
```

Both images run as `nobody` (UID 65534), no shell, no OS userland.

**→ docker-compose, systemd, Kubernetes, production checklist:** [docs/deployment.md](docs/deployment.md)

---

## Editor Integration (JSON Schema)

Conduit ships a [JSON Schema](schema/conduit.schema.json) for autocompletion,
hover docs, and inline validation in both JSON and YAML configs.

**VS Code — JSON: add one line to your config:**

```json
{
  "$schema": "https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json",
  "port": 3000
}
```

**VS Code — JSON: workspace-wide (all `conduit*.json` files):**

```json
// .vscode/settings.json
{
  "json.schemas": [{
    "fileMatch": ["conduit.json", "conduit.*.json"],
    "url": "https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json"
  }]
}
```

**VS Code — YAML: add to `.vscode/settings.json`** (requires the
[YAML extension](https://marketplace.visualstudio.com/items?itemName=redhat.vscode-yaml)):

```json
// .vscode/settings.json
{
  "yaml.schemas": {
    "https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json":
      ["conduit.yaml", "conduit.yml", "conduit.*.yaml"]
  }
}
```

**IntelliJ / WebStorm:** Settings → Languages & Frameworks → Schemas and DTDs → JSON Schema Mappings
→ add URL `https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json`,
file pattern `conduit*.json, conduit*.yaml`.

---

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.

- **Bug reports** → [GitHub Issues](https://github.com/lopatnov/conduit/issues)
- **Security vulnerabilities** → [GitHub Security Advisories](https://github.com/lopatnov/conduit/security/advisories) (not public issues)
- **Questions & ideas** → [GitHub Discussions](https://github.com/lopatnov/conduit/discussions)
- **Found it useful?** — a ⭐ helps others discover the project

---

## License

[Apache 2.0](LICENSE) © 2024–2026 [Oleksandr Lopatnov](https://github.com/lopatnov)
