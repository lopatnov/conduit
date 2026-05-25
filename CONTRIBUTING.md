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
│   ├── validate.rs      semantic validation
│   ├── env.rs           $VAR interpolation
│   └── defaults.rs      Default impls
├── server/
│   ├── builder.rs       Pingora bootstrap
│   ├── tls.rs           TLS settings (rustls)
│   ├── redirect.rs      HTTP→HTTPS redirect proxy
│   └── shutdown.rs      graceful shutdown
├── proxy/
│   ├── service.rs       ConduitProxy (ProxyHttp impl)
│   ├── router.rs        host + path routing
│   ├── ctx.rs           RequestCtx, UpstreamTarget
│   ├── upstream.rs      load balancer, URL parsing
│   └── cache.rs         ConduitCacheKey
├── handler/
│   ├── response.rs      write_local_response() helper
│   ├── static_files.rs  ETag, Range, Cache-Control
│   ├── health.rs        /__health__
│   ├── metrics.rs       /__metrics__ (Prometheus)
│   ├── hot_reload.rs    SSE hot reload
│   └── fallback.rs      fallback responses
├── filter/
│   ├── auth.rs          Basic Auth, API key
│   ├── compression.rs   gzip / brotli
│   ├── cors.rs          CORS + preflight
│   ├── headers.rs       custom response headers
│   ├── ip_filter.rs     CIDR allow/deny
│   ├── limits.rs        body/header size limits
│   ├── logging.rs       access log
│   ├── rate_limit.rs    token bucket
│   ├── redirects.rs     path redirects
│   ├── response_time.rs X-Response-Time
│   └── security_headers.rs  HSTS, CSP, etc.
├── admin/
│   └── api.rs           Admin API (Axum)
├── upload/
│   └── server.rs        upload server (Axum loopback)
└── util/
    ├── log_writer.rs    atomic log file writer
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
