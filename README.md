# Conduit

[![CI](https://github.com/lopatnov/conduit/actions/workflows/ci.yml/badge.svg)](https://github.com/lopatnov/conduit/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![crates.io downloads](https://img.shields.io/crates/d/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
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
- [Configuration Recipes](#configuration-recipes)
- [Admin API](#admin-api)
- [Docker](#docker)
- [Deployment](docs/deployment.md) ↗
- [Editor Integration (JSON Schema)](#editor-integration-json-schema)
- [Benchmarks](#benchmarks)
- [Contributing](#contributing)
- [Built With](#built-with)
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

| URL                                            | Description                                                  |
| ---------------------------------------------- | ------------------------------------------------------------ |
| [http://localhost:8080](http://localhost:8080) | Public app — proxy, cache, compression, rate limiting        |
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
conduit upstreams add    --route PATH --target URL [--weight N] [--site LABEL]
conduit upstreams remove --route PATH --target URL [--site LABEL]
conduit upstreams weight --route PATH --target URL --weight N [--site LABEL]
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

Conduit reads `conduit.json` (or `conduit.yaml` / `conduit.yml`) by default.
Pass `-c path/to/file` to use another file.

For the full configuration reference see **[docs/configuration.md](docs/configuration.md)**.

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
  "proxy": {
    "/api": { "targets": ["http://api:4000"], "strategy": "least-conn" }
  },
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
    "/users": "http://users-svc:4001",
    "/orders": "http://orders-svc:4002",
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
        { "url": "http://normal:4000", "weight": 1 }
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

| Endpoint            | Method | Description                               |
| ------------------- | ------ | ----------------------------------------- |
| `/status`           | GET    | Server version, uptime, inflight requests |
| `/reload`           | POST   | Hot-reload config from disk               |
| `/shutdown`         | POST   | Graceful shutdown                         |
| `/upstreams`        | GET    | Health, latency, and weights per backend  |
| `/upstreams/add`    | POST   | Add an upstream (in memory only)          |
| `/upstreams/remove` | POST   | Remove an upstream                        |
| `/upstreams/weight` | POST   | Change a backend's weight (WRR only)      |

Dynamic upstream changes survive until `conduit reload` — which resets from the config file.

**Request body fields for `/upstreams/add`, `/upstreams/remove`, `/upstreams/weight`:**

| Field    | Required | Description                                                                              |
| -------- | -------- | ---------------------------------------------------------------------------------------- |
| `route`  | ✅        | Route path, e.g. `"/api"`                                                                |
| `target` | ✅        | Full upstream URL, e.g. `"http://b3:4000"`                                               |
| `weight` | add/weight | Target weight (default: 1 for add)                                                    |
| `site`   | —        | Site label to scope the change, e.g. `"app.example.com:443"`. Omit to apply to all sites with this route. |

---

## Docker

Official images are published to the **GitHub Container Registry** on every tagged release:

```bash
docker pull ghcr.io/lopatnov/conduit:latest
docker pull ghcr.io/lopatnov/conduit:1.0.0
```

The image is built from [`contrib/Dockerfile`](contrib/Dockerfile) — a multi-stage build
that compiles a fully-static musl binary and packages it into a `FROM scratch` image (~14 MB).
It runs as UID 65534 (`nobody`) with no shell or OS userland.

### Run

```bash
docker run -p 8080:8080 \
  -v $(pwd)/conduit.json:/etc/conduit/conduit.json:ro \
  -v $(pwd)/dist:/dist:ro \
  ghcr.io/lopatnov/conduit
```

### docker-compose

```yaml
services:
  conduit:
    image: ghcr.io/lopatnov/conduit:latest
    ports:
      - "443:443"
      - "80:80"
    volumes:
      - ./conduit.json:/etc/conduit/conduit.json:ro
      - ./dist:/dist:ro
      - ./certs:/certs
    environment:
      METRICS_TOKEN: "${METRICS_TOKEN}"
    restart: unless-stopped

  api:
    image: my-api:latest
    expose: ["4000"]
```

### Build your own image

```bash
git clone https://github.com/lopatnov/conduit
cd conduit
docker build -f contrib/Dockerfile -t conduit:local .
```

---

## Editor Integration (JSON Schema)

Conduit ships a [JSON Schema](schema/conduit.schema.json) that enables autocompletion,
hover documentation, and inline validation in any JSON-aware editor.

### VS Code — automatic (recommended)

Add one line to your `conduit.json`:

```json
{
  "$schema": "https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json",
  "port": 3000
}
```

VS Code picks up `$schema` automatically — no extension needed.

### VS Code — workspace-wide

Add to `.vscode/settings.json` to enable validation for **all** `conduit*.json` files in
the workspace without adding `$schema` to every file:

```json
{
  "json.schemas": [
    {
      "fileMatch": ["conduit.json", "conduit.*.json"],
      "url": "https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json"
    }
  ]
}
```

### IntelliJ / WebStorm

**Settings → Languages & Frameworks → Schemas and DTDs → JSON Schema Mappings**

| Field             | Value                                                                                |
| ----------------- | ------------------------------------------------------------------------------------ |
| Schema URL        | `https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json` |
| Schema version    | JSON Schema version 2020-12                                                          |
| File path pattern | `conduit*.json`                                                                      |

### Any other editor

Use the schema URL directly — most modern editors that support JSON Schema accept a `$schema`
property or a manual mapping:

```text
https://raw.githubusercontent.com/lopatnov/conduit/main/schema/conduit.schema.json
```

---

## Benchmarks

See [docs/benchmarks.md](docs/benchmarks.md) for methodology and results.

---

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

- **Bug reports** → [GitHub Issues](https://github.com/lopatnov/conduit/issues)
- **Security vulnerabilities** → [GitHub Security Advisories](https://github.com/lopatnov/conduit/security/advisories) (do not use public issues)
- **Questions & ideas** → [GitHub Discussions](https://github.com/lopatnov/conduit/discussions)
- **Found it useful?** — a ⭐ on GitHub helps others discover the project

---

## Built With

### Core runtime

![Rust](https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white)
![Cloudflare Pingora](https://img.shields.io/badge/Cloudflare_Pingora_0.8-F48120?style=flat&logo=cloudflare&logoColor=white)
![Tokio](https://img.shields.io/badge/Tokio-000000?style=flat&logo=tokio&logoColor=white)
![Axum](https://img.shields.io/badge/Axum_0.8-000000?style=flat)

### TLS & certificates

![rustls](https://img.shields.io/badge/rustls-5C5C5C?style=flat)
![rcgen](https://img.shields.io/badge/rcgen-5C5C5C?style=flat)
![instant-acme](https://img.shields.io/badge/instant--acme-5C5C5C?style=flat)

### Configuration & parsing

![serde](https://img.shields.io/badge/serde-5C5C5C?style=flat)
![serde_json](https://img.shields.io/badge/serde__json-5C5C5C?style=flat)
![indexmap](https://img.shields.io/badge/indexmap-5C5C5C?style=flat)
![humantime](https://img.shields.io/badge/humantime-5C5C5C?style=flat)
![serde_path_to_error](https://img.shields.io/badge/serde__path__to__error-5C5C5C?style=flat)

### Performance & concurrency

![arc-swap](https://img.shields.io/badge/arc--swap-5C5C5C?style=flat)
![dashmap](https://img.shields.io/badge/dashmap-5C5C5C?style=flat)
![async-compression](https://img.shields.io/badge/async--compression_(brotli_·_gzip_·_deflate)-5C5C5C?style=flat)

### Middleware & scripting

![Rhai](https://img.shields.io/badge/Rhai_scripting-5C5C5C?style=flat)
![Wasmtime](https://img.shields.io/badge/Wasmtime_(WASM_plugins)-5C5C5C?style=flat)
![regex](https://img.shields.io/badge/regex-5C5C5C?style=flat)
![Redis](https://img.shields.io/badge/Redis_(rate_limit_store)-DC382D?style=flat&logo=redis&logoColor=white)

### File handling

![notify](https://img.shields.io/badge/notify_(fs_watcher)-5C5C5C?style=flat)
![multer](https://img.shields.io/badge/multer_(multipart)-5C5C5C?style=flat)
![uuid](https://img.shields.io/badge/uuid_v4-5C5C5C?style=flat)
![mime_guess](https://img.shields.io/badge/mime__guess-5C5C5C?style=flat)

### CLI & UX

![clap](https://img.shields.io/badge/clap_4_(derive)-5C5C5C?style=flat)
![clap_complete](https://img.shields.io/badge/clap__complete_(bash_·_zsh_·_fish)-5C5C5C?style=flat)
![clap_mangen](https://img.shields.io/badge/clap__mangen_(man_page)-5C5C5C?style=flat)
![dialoguer](https://img.shields.io/badge/dialoguer_(init_wizard)-5C5C5C?style=flat)
![indicatif](https://img.shields.io/badge/indicatif_(progress_bars)-5C5C5C?style=flat)

### Observability

![tracing](https://img.shields.io/badge/tracing_+_tracing--subscriber-5C5C5C?style=flat)
![Prometheus](https://img.shields.io/badge/Prometheus-E6522C?style=flat&logo=prometheus&logoColor=white)

### Dev tools & CI

![GitHub Actions](https://img.shields.io/badge/GitHub_Actions-2088FF?style=flat&logo=githubactions&logoColor=white)
![Docker](https://img.shields.io/badge/Docker_(musl_+_scratch)-2496ED?style=flat&logo=docker&logoColor=white)
![cross](https://img.shields.io/badge/cross_(cross--compilation)-5C5C5C?style=flat)
![reqwest](https://img.shields.io/badge/reqwest_(integration_tests)-5C5C5C?style=flat)
![criterion](https://img.shields.io/badge/criterion_(benchmarks)-5C5C5C?style=flat)
![serial_test](https://img.shields.io/badge/serial__test-5C5C5C?style=flat)
![SonarCloud](https://img.shields.io/badge/SonarCloud-F3702A?style=flat&logo=sonarcloud&logoColor=white)
![CodeRabbit](https://img.shields.io/badge/CodeRabbit-FF7A00?style=flat)

---

## License

[Apache 2.0](LICENSE) © 2024–2026 [Oleksandr Lopatnov](https://github.com/lopatnov)
