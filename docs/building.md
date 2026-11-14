# Building Conduit from Source

## Prerequisites

- **Rust stable** toolchain — [rustup.rs](https://rustup.rs)
- **MSRV 1.89** for the default and `standard` builds (set via
  `rust-version` in the workspace root's `[workspace.package]`, inherited by
  every member crate). `--features kubernetes` needs **1.95** (kube 5.1;
  `crates/conduit-k8s` overrides its own `rust-version`) and `--features wasm`
  (and so `full`) needs **1.97** — `crates/conduit-plugin-wasm` overrides its
  own `rust-version` for wasmtime 50's floor. CI's main jobs build on current
  `stable`, which is ahead of all three; the `msrv` CI job checks the default
  and `standard` builds on the 1.89 floor, and `msrv-k8s` the `kubernetes` build on 1.95. Running the test suite needs a newer toolchain than
  1.89 (a dev-dependency, `serial_test 4`, requires 1.93.1). The MSRV numbers
  only matter if you pin an older toolchain yourself.

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
(`default = ["proxy", "compression", "static", "hotreload"]`) — core reverse proxy, TLS, static files, rate
limiting, basic/API-key auth, compression, hot-reload, Prometheus metrics,
health checks, and the Admin API. See [Optional features](#optional-features)
below for the `standard` bundle that matches the published binaries and
Docker images.

---

## Optional features

`conduit -c conduit.yaml features` prints the features a configuration needs and a ready-to-paste `cargo install`
line (see [CLI reference](cli.md#features)).

The default build (`default = ["proxy", "compression", "static", "hotreload"]`) is the minimal embed-friendly
proxy. Add features with `--features`:

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
| `cache`         | Response caching (`proxy.*.cache`); implies `proxy`                        |
| `disk-cache`    | Disk-backed cache store (`cache.store: "disk:/path"`)                      |
| `acme`          | Auto-TLS via Let's Encrypt (`tls.acme`)                                    |
| `fault-injection` | Fault injection for chaos testing (`faultInjection`)                     |
| `otlp`          | OpenTelemetry OTLP distributed tracing (`global.otlp`)                     |
| `tokio-metrics` | `conduit_eventloop_lag_ms` Prometheus gauge (no config key)                |
| `kubernetes`    | Kubernetes CRD config provider (`--kubernetes-namespace`)                  |
| `standard`      | Bundle: `jwt` + `consumers` + `forward-auth` + `cache` + `disk-cache` + `acme` — typical self-hosted reverse-proxy / API-gateway set |
| `static-server` | Bundle for `--no-default-features`: `static` + `compression` + `hotreload` — the default set minus `proxy` |
| `gateway`       | Bundle for `--no-default-features`: `proxy` + `jwt` + `consumers` + `forward-auth` + `cache` + `disk-cache` + `acme` + `compression` — the `standard` set without static files and hot-reload |
| `full`          | Every optional feature above (`static-server` and `gateway` are shorthands, not extra capabilities) |

`proxy`, `compression`, `static` and `hotreload` are on by default and are not
listed above because there is nothing to add — `--no-default-features` turns
them off. `static-server` and `gateway` are shorthands for the two useful
things to switch back on afterwards; they add nothing to a default build.

```bash
# A static-file server: no reverse proxy and none of its dependencies
# (reqwest, url, hmac, sha2); the Admin API and its /upstreams* endpoints stay
cargo build --release --no-default-features --features static-server

# An API gateway: reverse proxy + auth + cache + auto-TLS, but no static files
cargo build --release --no-default-features --features gateway
```

### Building without `proxy`

**Without `proxy`, reverse proxying is compiled out.** `proxy` and
`routes[].proxy` are ignored, and Conduit logs a startup warning naming each
ignored entry (also returned in the `warnings` array of `POST /reload`). Static
files, `upload`, `tcp` sites, the Admin API, `/metrics` and hot-reload keep
working.

**Who is affected:** only `--no-default-features` builds. `default`, `standard`
and `full` all include `proxy`; their dependency sets and behavior are
unchanged.

**Three config shapes change meaning** without `proxy` — check for them before
switching:

1. A legacy top-level `proxy` shorthand (`proxy: "http://…"`) next to a
   site-level `static`. With `proxy`, the proxy wins and the `static` root is
   shadowed — it is never served. Without `proxy` the shadow disappears and
   **the previously dead `static` root becomes live**, so a directory that was
   never reachable is now served.
2. A legacy `proxy` map (`proxy: { "/api": "http://…" }`) next to a site-level
   `static`. Requests under the proxied prefixes used to go to an upstream; they
   now fall through to `static`, then to `fallback`.
3. A `routes[]` entry with a `proxy` action. It ends in the site's `fallback`
   response — not in that entry's own `static` half, which stays dead
   configuration.

**What leaves the binary** (`--no-default-features --features static`: 302 → 265
crates): the proxy-only code (upstream selection with capacity limits,
slow-start and sticky sessions, retry, traffic mirroring, active health
checks), the sticky-session crypto (`hmac`, `sha2`), the HTTP client used for
traffic mirroring and cache early-refresh (`reqwest` with its
`hyper-rustls`/`tower-http` layers) and the URL parser (`url` with its
`idna`/`icu_*` tree). Features that need them bring them back: `proxy` brings
back all of it, and so does `cache` (it implies `proxy`), while `forward-auth`
and `jwt` each bring back `reqwest`, `url` and `tower-http` through their own
crates (`jwt` also `hmac`/`sha2`) — so a build that enables any of those is
larger than the figures above. `--no-default-features --features static-server`
is the shortest way to get the default set minus `proxy`; it counts a few more
crates than the `static`-only figures above because it also keeps `compression`
and `hotreload`.

**What stays:** the Admin API (axum) and Pingora still need the
`hyper`/`tower`/`h2` stack, plus `base64`, `subtle`, `regex`, `dashmap`, `notify`
and `prometheus`, so this is not a "no HTTP stack" build. The `/upstreams*` admin
endpoints and the `conduit upstreams …` / `status --upstream` commands stay
available — they are plain HTTP clients of an admin API, which may belong to
another instance — but no active health checks run, so the upstream registry
holds no probe results. `DELETE /cache/purge` answers `501` without `cache`.

Tracking: [#144](https://github.com/lopatnov/conduit/issues/144).

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

# Linux ARM64 musl — a static binary for ARM64 containers
cross build --release --target aarch64-unknown-linux-musl

# Linux RISC-V 64 (the full build, as in the release; cross compiles it from x86-64)
cross build --release --target riscv64gc-unknown-linux-gnu --features full

# 32-bit Linux: x86, ARMv7 (Raspberry Pi 2 and later with a 32-bit OS), ARMv6 (Raspberry Pi 1 and Zero), soft-float ARM
cross build --release --target i686-unknown-linux-gnu --features full
cross build --release --target armv7-unknown-linux-gnueabihf --features full
cross build --release --target arm-unknown-linux-gnueabihf --features full
cross build --release --target arm-unknown-linux-gnueabi --features full
```

> **macOS targets** can only be built on macOS, and **Windows ARM64**
> (`aarch64-pc-windows-msvc`) on Windows (an ARM64 host is simplest).
> The [release workflow](../.github/workflows/release.yml) uses GitHub-hosted
> macOS and Windows runners for those targets.

### Supported targets

| Target                          | Tier   | Standard | Full |
| ------------------------------- | ------ | :------: | :--: |
| `x86_64-unknown-linux-gnu`      | Tier 1 | ✅       | ✅   |
| `x86_64-unknown-linux-musl`     | Tier 2 | ✅       | ✅   |
| `aarch64-unknown-linux-gnu`     | Tier 1 | ✅       | ✅   |
| `aarch64-unknown-linux-musl`    | Tier 2 | ✅       | ✅   |
| `riscv64gc-unknown-linux-gnu`   | Tier 2 | ✅       | ✅   |
| `x86_64-apple-darwin`           | Tier 2 | ✅       | ✅   |
| `aarch64-apple-darwin`          | Tier 1 | ✅       | ✅   |
| `x86_64-pc-windows-msvc`        | Tier 1 | ✅       | ✅   |
| `aarch64-pc-windows-msvc`       | Tier 1 | ✅       | ✅   |
| `i686-unknown-linux-gnu`        | Tier 1 | ✅       | ✅   |
| `armv7-unknown-linux-gnueabihf` | Tier 2 | ✅       | ✅   |
| `arm-unknown-linux-gnueabihf`   | Tier 2 | ✅       | ✅   |
| `arm-unknown-linux-gnueabi`     | Tier 2 | ✅       | ✅   |

The tier is the one the [Rust platform support page](https://doc.rust-lang.org/rustc/platform-support.html) gives the
target (it changes between Rust releases). A tick means the build is in the release. How far each is tested: CI runs the
workspace tests on native runners (Linux x86-64 and ARM64 with glibc, macOS on Apple Silicon, Windows x86-64 and
ARM64); the musl targets, RISC-V and macOS Intel are cross-compiled or built only (the musl binaries also go
through the Debian package tests); the 32-bit x86 Linux build runs the `standard` suite in CI, and its finished `full` binary is run in an
`i386/debian:12` container (it starts, serves a static file, proxies to another instance, takes an upload); the three
32-bit ARM builds compile and link in CI and were run once under QEMU with the `standard` suite: 1,028 tests passed
and 61 failed, all tests of the proxy path, because the emulator lacks the socket option Pingora sets on upstream
connections (the same tests pass on 32-bit x86).
A 32-bit x86 process in a Docker container cannot create a socket under the default seccomp profile of Docker 29.4.2
(measured: `Function not implemented`, and with it a container's DNS fails; Docker 28.0.4 is fine). Docker's write-up of
its fix for CVE-2026-31431 says why: 29.4.2 blocked the `socketcall` system call, which older 32-bit libc uses for every
socket operation (moby/moby#52506), and 29.4.3 reverted that and enforces the same limit in AppArmor and SELinux instead
(moby/moby#52537; we have not run 29.4.3). Options, safest first: use Docker 29.4.3 or later, or run the binary outside a
container. Both of the other two options allow `socketcall`, and through it `AF_ALG` sockets, which is what the 29.4.2
block was for: use them only on a kernel that includes the fix for CVE-2026-31431 (Docker's write-up says Debian and
RHEL 9 have one and Ubuntu did not yet when it was written; check yours), because on an unpatched kernel a compromised
container can use that bug to gain privileges inside the container. The first is the workaround that
Docker's 29.4.2 release notes recommend: the `seccomp/v0.2.1` profile of the `moby/profiles` repository, which allows
`socketcall` but keeps `AF_ALG` closed for `socket()` (the notes themselves say the `socketcall` path stays open, and
warn against the `v0.2.0` profile); start the container with `--security-opt seccomp=<that profile file>` (not run
here; we know it from the release notes). The second, a last resort that Docker's notes advise against, is
`--security-opt seccomp=unconfined`: it works (measured), but it turns off the container's whole syscall filter, one of
the limits on what an exploited network-facing process can do, so use it only for a throw-away or test container and
never for one that faces untrusted traffic (issue #708). The CI jobs that run 32-bit code in a container are pinned to `ubuntu-24.04`
(Docker 28) for this reason until the `ubuntu-latest` runner has Docker 29.4.3 or later.
`i686-pc-windows-msvc` is not in the table: `pingora-core` does not compile for 32-bit Windows yet
([cloudflare/pingora#1119](https://github.com/cloudflare/pingora/issues/1119)). Android does not build either
and is not supported by Pingora.

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
# Minimal image (default = ["proxy", "compression", "static", "hotreload"])
docker build -f contrib/Dockerfile -t conduit:local .

# Standard image (matches the published default tag)
docker build -f contrib/Dockerfile \
  --build-arg FEATURES=standard \
  -t conduit:local-standard .

# Full features image (the published :latest)
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
