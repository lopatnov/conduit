# Contributing to Conduit

Thank you for your interest in contributing! This document explains how to get started.

## Table of Contents

- [Development Setup](#development-setup)
- [Project Structure](#project-structure)
- [Running Tests](#running-tests)
- [Code Style](#code-style)
- [Submitting Changes](#submitting-changes)
- [Reporting Bugs](#reporting-bugs)
- [Requesting Features](#requesting-features)

---

## Development Setup

### Prerequisites

- Rust stable toolchain (`rustup toolchain install stable`)
- `rustfmt` and `clippy` components (`rustup component add rustfmt clippy`)

### Build

```bash
git clone https://github.com/lopatnov/conduit
cd conduit
cargo build
```

### Run

```bash
cargo run -- -c examples/minimal.json
```

---

## Project Structure

```text
src/
├── main.rs              entry point — CLI dispatch
├── cli/
│   ├── args.rs          clap CLI definitions
│   └── init.rs          conduit init wizard
├── config/
│   ├── schema.rs        all config types (serde)
│   ├── parse.rs         load_config(), from_str(), normalize()
│   ├── validate.rs      semantic validation + TLS cert expiry
│   ├── env.rs           $VAR interpolation
│   └── defaults.rs      Default impls
├── server/
│   ├── builder.rs       Pingora bootstrap
│   ├── tls.rs           TLS settings (rustls)
│   ├── acme.rs          Auto-TLS via instant-acme (Let's Encrypt)
│   ├── redirect.rs      HTTP→HTTPS redirect proxy
│   └── shutdown.rs      graceful shutdown
├── proxy/
│   ├── service.rs       ConduitProxy (ProxyHttp impl)
│   ├── router.rs        host + path routing, route table
│   ├── routes.rs        RouteConfig / MatchConfig (glob, method, header, query)
│   ├── ctx.rs           RequestCtx, UpstreamTarget, GuardCtx
│   ├── upstream.rs      load balancer, URL parsing, health registry
│   ├── health.rs        background health checks, UpstreamRegistry
│   ├── cache.rs         build_cache_key(), in-memory storage singleton
│   ├── cache_redis.rs   Redis-backed pingora-cache Storage impl
│   └── cache_disk.rs    Disk-backed pingora-cache Storage impl
├── handler/
│   ├── response.rs      write_local_response() helper
│   ├── static_files.rs  ETag, Range, Cache-Control, streaming compression
│   ├── health.rs        /__health__ with optional upstream status
│   ├── metrics.rs       /__metrics__ (Prometheus)
│   ├── hot_reload.rs    SSE browser hot reload + notify watcher
│   └── fallback.rs      fallback responses (byAccept, file, body)
├── filter/
│   ├── auth.rs          Basic Auth, API key, skip-paths
│   ├── compression.rs   gzip / brotli negotiation + streaming
│   ├── cors.rs          CORS + preflight (bypasses auth)
│   ├── headers.rs       custom response headers
│   ├── ip_filter.rs     CIDR allow/deny, X-Forwarded-For
│   ├── limits.rs        maxBodyBytes (413), maxHeaderBytes (431)
│   ├── logging.rs       5 log formats, atomic file switching
│   ├── rate_limit.rs    token-bucket, in-memory or Redis
│   ├── rate_limit_redis.rs  Redis fixed-window counter with fallback
│   ├── redirects.rs     path redirects with :param captures
│   ├── response_time.rs X-Response-Time header
│   ├── script.rs        Rhai scripting middleware (Phase 4)
│   └── security_headers.rs  HSTS, CSP, X-Frame-Options, etc.
├── admin/
│   └── api.rs           Admin API (Axum) — status, reload, upstream management
├── upload/
│   └── server.rs        upload server (Axum loopback, port 0)
└── util/
    ├── log_writer.rs    atomic log file writer (Arc<Mutex<Inner>>)
    ├── mime.rs          Content-Type detection
    ├── path.rs          path utilities
    └── net.rs           network utilities
```

---

## Running Tests

```bash
# All tests
cargo test

# A specific test file
cargo test --test config_parse

# A specific test
cargo test --test proxy tls_https

# With output
cargo test -- --nocapture

# Benchmarks
cargo bench
```

### Integration tests

Integration tests in `tests/` start a real Conduit process on a random port using
`tests/common/mod.rs`. They require the binary to be built first:

```bash
cargo test --test proxy
```

### Writing tests

- Unit tests live in the same file as the code (`#[cfg(test)]` module)
- Integration tests live in `tests/` and use `tests/common::TestServer`
- Use `port: 0` — the OS assigns a free port automatically
- Use `serial_test::serial` for Admin API tests (shared admin port)
- Use `rcgen` for in-memory TLS certificates

---

## Code Style

### Formatting

```bash
cargo fmt
```

All code must pass `cargo fmt --check` in CI.

### Linting

```bash
cargo clippy -- -D warnings
```

All code must pass clippy with zero warnings.

### General guidelines

- **English only** — all code, comments, commit messages, and docs must be in English
- Keep `src/main.rs` thin — it dispatches to modules, no business logic
- Prefer `thiserror` in library modules, `anyhow` at binary entry points
- Use `tracing::trace!` in hot paths — not `debug!` or higher
- No `unwrap()` in non-test code — use `?` or explicit error handling
- All `regex::Regex` values must be compiled once at startup, not per-request
- Do not use `once_cell` or `lazy_static` — use `std::sync::OnceLock` (Rust 1.70+)

---

## Submitting Changes

1. Fork the repository
2. Create a feature branch: `git checkout -b feat/my-feature`
3. Make your changes
4. Ensure all checks pass:

   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   ```

5. Commit with a clear message:

   ```text
   feat: add weighted round-robin load balancing

   Implements ProxyTarget::Weighted with static weights configured in
   conduit.json. The balance algorithm uses a Smooth Weighted Round-Robin
   (SWRR) to distribute traffic proportionally without clustering.
   ```
6. Push and open a Pull Request against `main`

### Commit message format

```text
<type>: <short summary>

<body — explain why, not what>
```

Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`

### PR checklist

- [ ] Tests cover the new behavior
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes on Linux, macOS, and Windows
- [ ] `conduit validate` works on affected example configs
- [ ] Docs updated (README, CLAUDE.md if architectural)

---

## Dependencies

### Core runtime

| Crate                                                           | Role                       |
| --------------------------------------------------------------- | -------------------------- |
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
| [async-compression](https://github.com/Nemo157/async-compression) | Brotli / gzip / zstd        |

### Middleware & scripting

| Crate                                             | Role                            |
| ------------------------------------------------- | ------------------------------- |
| [rhai](https://rhai.rs)                           | Embedded scripting engine       |
| [wasmtime](https://wasmtime.dev)                  | WASM plugin host                |
| [regex](https://github.com/rust-lang/regex)       | URL rewriting, header routing   |
| [reqwest](https://github.com/seanmonstar/reqwest) | Forward auth, JWKS, mirroring   |
| [redis](https://github.com/redis-rs/redis-rs)     | Distributed rate-limit / cache  |

### Auth & security

| Crate                                                 | Role                                 |
| ----------------------------------------------------- | ------------------------------------ |
| [jsonwebtoken](https://github.com/Keats/jsonwebtoken) | JWT validation (HS256, RS256, ES256) |
| [subtle](https://github.com/dalek-cryptography/subtle) | Constant-time credential comparison |
| [ipnet](https://github.com/krisprice/ipnet)           | CIDR-based IP filtering              |

### Observability

| Crate                                                                                      | Role                             |
| ------------------------------------------------------------------------------------------ | -------------------------------- |
| [tracing](https://github.com/tokio-rs/tracing) + tracing-subscriber                        | Structured logging               |
| [prometheus](https://github.com/tikv/rust-prometheus)                                      | Metrics exposition               |
| [opentelemetry](https://github.com/open-telemetry/opentelemetry-rust) + opentelemetry-otlp | OTLP tracing (`--features otlp`) |

### File handling & CLI

| Crate                                                 | Role                            |
| ----------------------------------------------------- | ------------------------------- |
| [notify](https://github.com/notify-rs/notify)         | Filesystem watcher (hot reload) |
| [uuid](https://github.com/uuid-rs/uuid)               | X-Request-ID generation         |
| [mime_guess](https://github.com/abonander/mime_guess) | Content-Type detection          |
| [clap 4](https://github.com/clap-rs/clap)             | CLI argument parsing            |
| [clap_complete](https://github.com/clap-rs/clap)      | Shell completion scripts        |
| [dialoguer](https://github.com/console-rs/dialoguer)  | `conduit init` wizard           |
| [indicatif](https://github.com/console-rs/indicatif)  | Progress bars (`conduit probe`) |

---

## Reporting Bugs

Open an issue at <https://github.com/lopatnov/conduit/issues>.

Include:
- Conduit version (`conduit --version`)
- OS and architecture
- Minimal `conduit.json` that reproduces the issue
- Expected vs actual behavior
- Relevant logs (`RUST_LOG=debug conduit`)

---

## Requesting Features

Open an issue with the `enhancement` label. Describe:

1. The problem you are trying to solve
2. Why existing config options do not cover it
3. A proposed JSON config snippet (if applicable)

Large features are tracked as phases in `CLAUDE.md`. If you want to work on a specific
phase, comment on the relevant issue to coordinate.
