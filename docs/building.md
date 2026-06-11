# Building Conduit from Source

## Prerequisites

- **Rust stable** toolchain — [rustup.rs](https://rustup.rs)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

No other system dependencies are required. All C libraries (TLS, compression)
are statically linked via `-sys` crates.

---

## Quick start

```bash
git clone https://github.com/lopatnov/conduit
cd conduit

# Debug — fast to compile, no optimisations
cargo build
./target/debug/conduit --version

# Release — LTO, stripped, production-ready (~14 MB on Linux musl)
cargo build --release
./target/release/conduit --version
# Windows: target\release\conduit.exe
```

`cargo build --release` with no flags produces the **minimal build**
(`default = []`) — core reverse proxy, TLS, static files, rate limiting,
basic/API-key auth, compression, hot-reload, Prometheus metrics, health
checks, and the Admin API. See [Optional features](#optional-features) below
for the `standard` bundle that matches the published binaries and Docker
images.

---

## Optional features

The default build (`default = []`) is the minimal embed-friendly proxy.
Add features with `--features`:

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
| `fault-injection` | Fault injection for chaos testing (`faultInjection`)                     |
| `otlp`          | OpenTelemetry OTLP distributed tracing (`global.otlp`)                     |
| `kubernetes`    | Kubernetes CRD config provider (`--kubernetes-namespace`)                  |
| `standard`      | Bundle: `jwt` + `consumers` + `forward-auth` + `cache` + `acme` — typical self-hosted reverse-proxy / API-gateway set |
| `full`          | All of the above                                                           |

```bash
# Typical self-hosted reverse-proxy / API gateway (auth stack + caching +
# auto-TLS) — matches the published "standard" binaries and Docker images
cargo build --release --features standard

# Custom production set
cargo build --release --features "jwt,rhai,redis,otlp"

# All features
cargo build --release --features full
```

See **[docs/cli.md — Build features](cli.md#build-features)** for binary sizes
and per-feature documentation.

---

## Release profile

The release pipeline uses these settings (`Cargo.toml`):

```toml
[profile.release]
lto           = true   # link-time optimisation across all crates
codegen-units = 1      # single codegen unit — slower to compile, faster binary
strip         = true   # strip debug symbols (ELF / Mach-O)
```

---

## Cross-compilation

Requires [cross](https://github.com/cross-rs/cross) and Docker:

```bash
cargo install cross

# Linux x86-64 musl — smallest binary, runs in Docker FROM scratch
cross build --release --target x86_64-unknown-linux-musl

# Linux ARM64 — Raspberry Pi 4/5, AWS Graviton, Apple M-series VMs
cross build --release --target aarch64-unknown-linux-gnu

# Linux RISC-V 64 (standard feature bundle — see release.yml)
cross build --release --target riscv64gc-unknown-linux-gnu --features standard
```

> **macOS targets** can only be built on macOS.
> The [release workflow](../.github/workflows/release.yml) uses GitHub-hosted
> macOS runners for those targets.

> **RISC-V full build** (`--features wasm`) is best compiled natively on a
> RISC-V host. wasmtime's Cranelift JIT requires additional toolchain
> configuration for cross-compilation from x86-64 to riscv64.

### Supported targets

| Target                          | Tier   | Standard | Full |
| ------------------------------- | ------ | :------: | :--: |
| `x86_64-unknown-linux-gnu`      | Tier 1 | ✅       | ✅   |
| `x86_64-unknown-linux-musl`     | Tier 1 | ✅       | ✅   |
| `aarch64-unknown-linux-gnu`     | Tier 1 | ✅       | ✅   |
| `riscv64gc-unknown-linux-gnu`   | Tier 2 | ✅       | ✅ ¹ |
| `x86_64-apple-darwin`           | Tier 1 | ✅       | ✅   |
| `aarch64-apple-darwin`          | Tier 1 | ✅       | ✅   |
| `x86_64-pc-windows-msvc`        | Tier 1 | ✅       | ✅   |

> ¹ Best built natively on a riscv64 host; cross-compilation with `--features wasm`
> requires additional wasmtime toolchain setup.

---

## Running tests

```bash
# Unit tests (fast, no network)
cargo test --lib

# All tests — default build (no --features)
cargo test

# All tests — standard feature bundle (matches published binaries)
cargo test --features standard

# All tests — full features
cargo test --features full

# One specific integration test file
cargo test --test proxy

# With log output
cargo test -- --nocapture

# Benchmarks (criterion)
cargo bench
```

> Integration tests start a real Conduit process on a random port (`port: 0`).
> The binary must be compiled before the integration tests run — `cargo test`
> handles this automatically.

---

## Docker image

Build locally using the production Dockerfile (multi-stage musl + `FROM scratch`):

```bash
# Minimal image (default = [])
docker build -f contrib/Dockerfile -t conduit:local .

# Standard image (matches the published default tag)
docker build -f contrib/Dockerfile \
  --build-arg FEATURES=standard \
  -t conduit:local-standard .

# Full features image
docker build -f contrib/Dockerfile \
  --build-arg FEATURES=full \
  -t conduit:local-full .
```

See [docs/deployment.md](deployment.md) for Docker Compose and Kubernetes configs.

---

## Troubleshooting

**`ring` fails to compile on a new platform**
`ring` requires a C compiler and `perl` for its assembly routines. Install:
```bash
# Debian/Ubuntu
sudo apt install gcc perl
# Alpine
apk add gcc musl-dev perl
```

**`wasmtime` takes very long to compile**
Expected — Cranelift (the JIT backend) is large. Use `sccache` or Swatinem/rust-cache
in CI. First build takes ~5 minutes on a 4-core machine; subsequent builds are cached.

**Cross-compilation fails for musl target**
Ensure the musl linker is installed: `sudo apt install musl-tools`. With `cross`,
this is handled automatically by the Docker image.
