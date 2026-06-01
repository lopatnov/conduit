# Conduit

[![CI](https://github.com/lopatnov/conduit/actions/workflows/ci.yml/badge.svg)](https://github.com/lopatnov/conduit/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![crates.io downloads](https://img.shields.io/crates/d/lopatnov-conduit.svg)](https://crates.io/crates/lopatnov-conduit)
[![npm version](https://img.shields.io/npm/v/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![npm downloads](https://img.shields.io/npm/dt/@lopatnov/conduit.svg)](https://www.npmjs.com/package/@lopatnov/conduit)
[![GitHub stars](https://img.shields.io/github/stars/lopatnov/conduit)](https://github.com/lopatnov/conduit/stargazers)
[![GitHub issues](https://img.shields.io/github/issues/lopatnov/conduit)](https://github.com/lopatnov/conduit/issues)
[![License](https://img.shields.io/badge/license-Apache_2.0-blue.svg)](LICENSE)

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
- [Live Demo](docs/live-demo.md) ↗
- [Installation](#installation)
- [Building from Source](#building-from-source)
- [CLI Commands](#cli-commands) / [CLI Reference](docs/cli.md) ↗
- [Configuration](#configuration)
- [Configuration Recipes](#configuration-recipes) / [All recipes](docs/recipes.md) ↗
- [Admin API](#admin-api)
- [Docker / Deployment](docs/deployment.md) ↗
- [Editor Integration (JSON Schema)](#editor-integration-json-schema)
- [Benchmarks](docs/benchmarks.md) ↗
- [Contributing](#contributing)
- [Built With](#built-with)
- [License](#license)

---

## Quick Start

```bash
# 1. Create a config with the interactive wizard
#    (asks: YAML or JSON? port? static dir? proxy? TLS? …)
conduit init

# 2. Start
conduit

# 3. Validate before deploying to production
conduit validate
```

Or write a config by hand — both formats are equivalent:

```json
// conduit.json
{
  "port": 3000,
  "static": "./dist",
  "proxy": { "/api": "http://localhost:4000" }
}
```

See [`examples/minimal.yaml`](examples/minimal.yaml) for the annotated YAML version.

```text
GET /            → serves ./dist/index.html
GET /style.css   → serves ./dist/style.css
GET /api/users   → proxied to http://localhost:4000/api/users
```

> **Path forwarding:** by default the full path is forwarded — `/api/users` arrives
> at the upstream as `/api/users`. Add `stripPrefix: true` to strip the matched
> prefix so `/api/users` becomes `/users` on the upstream:
>
> ```json
> {
>   "proxy": {
>     "/api": { "targets": ["http://localhost:4000"], "stripPrefix": true }
>   }
> }
> ```
>
> This matches nginx's `proxy_pass http://backend/;` (trailing-slash) behaviour.
> Without `stripPrefix`, Conduit behaves like nginx's `proxy_pass http://backend;`
> (no trailing slash) — keeping the full path.

---

## Installation

> **Standard vs full binary:** All installation options below install the
> **standard** binary — no optional features (`otlp`, `wasm`, `kubernetes`).
> If you need OTLP tracing, WASM plugins, or Kubernetes CRD mode, use the
> **full binary** from [GitHub Releases](#option-3--pre-built-binaries) or
> [build from source](#optional-features) with `--features otlp,wasm,kubernetes`.

### Option 1 — npx (no installation, always latest)

```bash
npx @lopatnov/conduit           # start the server
npx @lopatnov/conduit init      # interactive setup wizard
npx @lopatnov/conduit validate  # validate config
```

Downloads the platform binary on first run, then caches it. Always uses the
**standard** binary. No optional features.

### Option 2 — npm global install

```bash
npm install -g @lopatnov/conduit
conduit           # start the server
conduit validate  # validate config
```

Installs the **standard** binary for your platform. No optional features.

### Option 3 — Pre-built binaries

Download from [GitHub Releases](https://github.com/lopatnov/conduit/releases).

Each release ships two variants per platform:

| Platform            | Standard                                   | Full (otlp + wasm + kubernetes)                 |
| ------------------- | ------------------------------------------ | ----------------------------------------------- |
| Linux x86-64        | `conduit-x86_64-unknown-linux-gnu.tar.gz`  | `conduit-x86_64-unknown-linux-gnu-full.tar.gz`  |
| Linux x86-64 musl   | `conduit-x86_64-unknown-linux-musl.tar.gz` | `conduit-x86_64-unknown-linux-musl-full.tar.gz` |
| Linux ARM64         | `conduit-aarch64-unknown-linux-gnu.tar.gz` | `conduit-aarch64-unknown-linux-gnu-full.tar.gz` |
| macOS Intel         | `conduit-x86_64-apple-darwin.tar.gz`       | `conduit-x86_64-apple-darwin-full.tar.gz`       |
| macOS Apple Silicon | `conduit-aarch64-apple-darwin.tar.gz`      | `conduit-aarch64-apple-darwin-full.tar.gz`      |
| Windows x86-64      | `conduit-x86_64-pc-windows-msvc.exe.zip`   | `conduit-x86_64-pc-windows-msvc-full.exe.zip`   |

```bash
# Linux — standard
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz && ./conduit --version

# Linux — full (OTLP + WASM + Kubernetes)
curl -L https://github.com/lopatnov/conduit/releases/latest/download/conduit-x86_64-unknown-linux-gnu-full.tar.gz \
  | tar xz && ./conduit --version
```

### Option 4 — Cargo

```bash
# Standard binary (no optional features)
cargo install lopatnov-conduit

# Full binary — enables OTLP tracing, WASM plugins, Kubernetes CRD mode
cargo install lopatnov-conduit --features otlp,wasm,kubernetes
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

### Optional features

The default binary includes everything except three optional features that add
compile-time dependencies:

| Feature      | Flag                    | What it enables                                           |
| ------------ | ----------------------- | --------------------------------------------------------- |
| `otlp`       | `--features otlp`       | OpenTelemetry OTLP tracing (`global.otlp` config)         |
| `wasm`       | `--features wasm`       | WebAssembly plugin middleware (`type: "wasm"`)            |
| `kubernetes` | `--features kubernetes` | Kubernetes CRD config provider (`--kubernetes-namespace`) |

```bash
# One feature
cargo build --release --features otlp

# Multiple features
cargo build --release --features otlp,wasm

# All optional features
cargo build --release --features otlp,wasm,kubernetes
```

See **[docs/cli.md — Build features](docs/cli.md#build-features)** for full
documentation of each feature.

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
conduit [-c FILE]                  start the server
conduit validate [-c FILE]         validate config — exit 0 OK, exit 1 errors
conduit fmt [--write] [-c FILE]    pretty-print / normalize config
conduit init [-o FILE]             interactive setup wizard
conduit probe [-c FILE]            ping every upstream, show latency table

conduit reload   [--admin ADDR]    hot-reload config without restarting
conduit status   [--admin ADDR]    show version, uptime, in-flight requests
conduit shutdown [--admin ADDR]    graceful shutdown
conduit upstreams [--admin ADDR]   list upstream health and latency
conduit upstreams add    --route PATH --target URL [--weight N] [--site LABEL]
conduit upstreams remove --route PATH --target URL [--site LABEL]
conduit upstreams weight --route PATH --target URL --weight N   [--site LABEL]

conduit completions bash|zsh|fish|powershell|elvish
conduit man                        generate man page (roff)
```

For detailed descriptions of every flag, argument, and exit code see
**[docs/cli.md](docs/cli.md)**.

---

## Configuration

All options are optional unless noted. Fields accept environment variable references —
`"$VAR"` is replaced with the value of `VAR` at startup.

Conduit reads `conduit.json` (or `conduit.yaml` / `conduit.yml`) by default.
Pass `-c path/to/file` to use another file.

For the full configuration reference see **[docs/configuration.md](docs/configuration.md)**.

---

## Configuration Recipes

Both **YAML** and **JSON** are fully supported. JSON is shown below for
compactness; click the YAML link to see the annotated version with comments.

### Local dev server

YAML with comments: [`examples/dev-hot-reload.yaml`](examples/dev-hot-reload.yaml)

```json
{
  "port": 3000,
  "logging": "dev",
  "cors": true,
  "hotReload": true,
  "static": "./src",
  "proxy": { "/api": "http://localhost:4000" },
  "fallback": { "file": "./src/index.html", "status": 200 }
}
```

### Auto-TLS + SPA (production)

YAML with comments: [`examples/spa-with-api.yaml`](examples/spa-with-api.yaml)

```json
{
  "port": 443,
  "tls": {
    "acme": {
      "email": "admin@example.com",
      "storage": "/var/cache/conduit/certs"
    },
    "httpRedirectPort": 80
  },
  "http2": true,
  "securityHeaders": true,
  "compression": true,
  "static": "./dist",
  "staticOptions": { "preCompressed": true, "maxAge": "1y" },
  "proxy": {
    "/api": {
      "targets": ["http://api1:4000", "http://api2:4000"],
      "strategy": "least-conn",
      "stripPrefix": true,
      "healthCheck": { "path": "/health" },
      "cache": { "store": "memory", "ttlSecs": 60, "skipIfCookie": true }
    }
  },
  "rateLimit": { "windowSecs": 60, "limit": 300 },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__" },
  "fallback": {
    "byAccept": {
      "html": { "file": "./dist/index.html", "status": 200 },
      "json": { "body": { "error": "Not Found" }, "status": 404 }
    }
  }
}
```

### Microservices gateway

YAML with comments: [`examples/api-gateway.yaml`](examples/api-gateway.yaml)

```json
{
  "port": 8080,
  "ipFilter": { "allow": ["10.0.0.0/8"] },
  "rateLimit": { "windowSecs": 60, "limit": 500 },
  "proxy": {
    "/users": "http://users-svc:4001",
    "/orders": "http://orders-svc:4002",
    "/catalog": {
      "targets": ["http://catalog1:4003", "http://catalog2:4003"],
      "cache": { "store": "memory", "ttlSecs": 300 }
    }
  },
  "healthCheck": true,
  "metrics": { "path": "/__metrics__" },
  "maskErrors": true
}
```

**→ More recipes:** [docs/recipes.md](docs/recipes.md) —
HTTPS, JWT, load balancing, failover, security hardening, observability, multi-site.

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

| Field    | Required   | Description                                                                                               |
| -------- | ---------- | --------------------------------------------------------------------------------------------------------- |
| `route`  | ✅         | Route path, e.g. `"/api"`                                                                                 |
| `target` | ✅         | Full upstream URL, e.g. `"http://b3:4000"`                                                                |
| `weight` | add/weight | Target weight (default: 1 for add)                                                                        |
| `site`   | —          | Site label to scope the change, e.g. `"app.example.com:443"`. Omit to apply to all sites with this route. |

---

## Docker

Two image variants are published on every release:

| Image    | Tag                           | Features                       |
| -------- | ----------------------------- | ------------------------------ |
| Standard | `:latest`, `:1.0.0`           | No optional features           |
| Full     | `:latest-full`, `:1.0.0-full` | `otlp` + `wasm` + `kubernetes` |

```bash
# Standard (~14 MB)
docker pull ghcr.io/lopatnov/conduit:latest

# Full — with OTLP tracing, WASM plugins, Kubernetes CRD support
docker pull ghcr.io/lopatnov/conduit:latest-full

docker run -p 8080:8080 \
  -v $(pwd)/conduit.yaml:/etc/conduit/conduit.yaml:ro \
  ghcr.io/lopatnov/conduit:latest -c /etc/conduit/conduit.yaml
```

Both images are built from `FROM scratch`, run as `nobody` (UID 65534).

**docker-compose, systemd, Kubernetes, and production checklist →** [docs/deployment.md](docs/deployment.md)

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

Methodology, test setup, and results: **[docs/benchmarks.md](docs/benchmarks.md)**

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

| Crate                                                           | Role                       |
| --------------------------------------------------------------- | -------------------------- |
| [Rust](https://rust-lang.org)                                   | Systems language           |
| [Cloudflare Pingora 0.8](https://github.com/cloudflare/pingora) | Async HTTP proxy framework |
| [Tokio](https://tokio.rs)                                       | Async runtime              |
| [Axum 0.8](https://github.com/tokio-rs/axum)                    | Admin API HTTP server      |

### TLS & certificates

| Crate                                                        | Role                           |
| ------------------------------------------------------------ | ------------------------------ |
| [rustls](https://github.com/rustls/rustls)                   | TLS implementation             |
| [rcgen](https://github.com/rustls/rcgen)                     | Certificate generation (tests) |
| [instant-acme](https://github.com/instant-labs/instant-acme) | ACME / Let's Encrypt client    |

### Configuration & parsing

| Crate                                                                      | Role                         |
| -------------------------------------------------------------------------- | ---------------------------- |
| [serde](https://serde.rs) + [serde_json](https://github.com/serde-rs/json) | Serialization                |
| [serde_yaml](https://github.com/dtolnay/serde-yaml)                        | YAML config format           |
| [serde_path_to_error](https://github.com/dtolnay/path-to-error)            | Precise parse error messages |
| [indexmap](https://github.com/bluss/indexmap)                              | Ordered map for route config |

### Performance & concurrency

| Crate                                                             | Role                        |
| ----------------------------------------------------------------- | --------------------------- |
| [arc-swap](https://github.com/vorner/arc-swap)                    | Lock-free hot reload        |
| [dashmap](https://github.com/xacrimon/dashmap)                    | Concurrent rate-limit state |
| [async-compression](https://github.com/Nemo157/async-compression) | Brotli / gzip / deflate     |

### Middleware & scripting

| Crate                                             | Role                          |
| ------------------------------------------------- | ----------------------------- |
| [rhai](https://rhai.rs)                           | Embedded scripting engine     |
| [wasmtime](https://wasmtime.dev)                  | WASM plugin host              |
| [regex](https://github.com/rust-lang/regex)       | URL rewriting                 |
| [reqwest](https://github.com/seanmonstar/reqwest) | Forward auth, JWKS, mirroring |
| [redis](https://github.com/redis-rs/redis-rs)     | Distributed rate-limit store  |

### Auth & security

| Crate                                                 | Role                                 |
| ----------------------------------------------------- | ------------------------------------ |
| [jsonwebtoken](https://github.com/Keats/jsonwebtoken) | JWT validation (HS256, RS256, ES256) |
| [ipnet](https://github.com/krisprice/ipnet)           | CIDR-based IP filtering              |

### File handling & CLI

| Crate                                                 | Role                            |
| ----------------------------------------------------- | ------------------------------- |
| [notify](https://github.com/notify-rs/notify)         | Filesystem watcher (hot reload) |
| [uuid](https://github.com/uuid-rs/uuid)               | X-Request-ID generation         |
| [mime_guess](https://github.com/abonander/mime_guess) | Content-Type detection          |
| [clap 4](https://github.com/clap-rs/clap)             | CLI argument parsing            |
| [clap_complete](https://github.com/clap-rs/clap)      | Shell completion scripts        |
| [dialoguer](https://github.com/console-rs/dialoguer)  | `conduit init` wizard           |
| [indicatif](https://github.com/console-rs/indicatif)  | Progress bars                   |

### Observability

| Crate                                                                                      | Role                             |
| ------------------------------------------------------------------------------------------ | -------------------------------- |
| [tracing](https://github.com/tokio-rs/tracing) + tracing-subscriber                        | Structured logging               |
| [prometheus](https://github.com/tikv/rust-prometheus)                                      | Metrics exposition               |
| [opentelemetry](https://github.com/open-telemetry/opentelemetry-rust) + opentelemetry-otlp | OTLP tracing (`--features otlp`) |

### Dev & CI

| Tool                                                  | Role                    |
| ----------------------------------------------------- | ----------------------- |
| [GitHub Actions](https://github.com/features/actions) | CI / release pipeline   |
| [Docker](https://docker.com) (musl + scratch)         | Minimal container image |
| [cross](https://github.com/cross-rs/cross)            | Cross-compilation       |
| [criterion](https://github.com/bheisler/criterion.rs) | Benchmarks              |
| [SonarCloud](https://sonarcloud.io)                   | Static analysis         |

---

## License

[Apache 2.0](LICENSE) © 2024–2026 [Oleksandr Lopatnov](https://github.com/lopatnov)
