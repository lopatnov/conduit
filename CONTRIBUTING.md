# Contributing to Conduit

Thank you for your interest in contributing! This document explains how to get started.

## Table of Contents

- [Development Setup](#development-setup)
- [Project Structure](#project-structure)
- [Cargo Workspace Crate Extraction Recipe](#cargo-workspace-crate-extraction-recipe)
- [Adding a New Feature](#adding-a-new-feature)
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
├── main.rs              entry point (~10 lines): init tracing, Cli::parse(), dispatch_command() — everything
│                        else moved to crates/conduit-cli (#147)
├── cli/
│   └── mod.rs           facade: every item re-exported from crates/conduit-cli (args.rs, dispatch.rs,
│                        init.rs, serve.rs, validate.rs, and the rest) — see crates/conduit-cli/src/lib.rs
├── config/
│   ├── schema/          facade: mod.rs re-exports every config type from crates/conduit-config (#222)
│   ├── parse.rs         facade: load_config(), from_str(), normalize() live in crates/conduit-config
│   ├── validate/        facade: mod.rs re-exports validate()/feature_warnings() from crates/conduit-server
│   │                    (#147) and keeps the root-vs-feature-crate compile-time parity asserts (cfg!() is
│   │                    always crate-relative, so those stay here); tests.rs/golden_tests.rs/testdata/ stay
│   │                    here too, since they pin the ROOT's own compiled feature set
│   ├── provider.rs      facade: FileProvider/Provider live in crates/conduit-server (#147)
│   ├── kubernetes.rs    facade: KubernetesProvider/CRD types live in crates/conduit-server (#147)
│   ├── env.rs           $VAR interpolation
│   └── defaults.rs      facade: the constants live in crates/conduit-server (#147)
├── server.rs            no facade — just the ten root-vs-`conduit-server` compile-time parity asserts
│                        (run_server()/AdminApiService/config::validate/the providers all moved there, #147)
├── proxy/               (the modules marked * are facades over crates/conduit-runtime since #145 — see below)
│   ├── service.rs *     ConduitProxy (ProxyHttp impl), AppState
│   ├── router.rs *      host + path routing, route table
│   ├── routes.rs        RouteConfig / MatchConfig (glob, method, header, query)
│   ├── ctx.rs *         RequestCtx, UpstreamTarget, GuardCtx
│   ├── upstream.rs      load balancer, URL parsing, health registry
│   ├── health.rs        background health checks, UpstreamRegistry
│   ├── cache.rs         build_cache_key(), in-memory storage singleton
│   ├── cache_redis.rs * Redis proxy-cache registry glue
│   └── cache_disk.rs    Disk-backed pingora-cache Storage impl
├── handler/
│   ├── response.rs      write_local_response() helper
│   ├── static_files.rs  ETag, Range, Cache-Control, streaming compression
│   ├── health.rs *      /__health__ with optional upstream status
│   ├── metrics.rs       /__metrics__ (Prometheus)
│   ├── hot_reload.rs    SSE browser hot reload + notify watcher
│   └── fallback.rs      fallback responses (byAccept, file, body)
├── filter/
│   ├── auth.rs *        Basic Auth, API key, skip-paths
│   ├── compression.rs   gzip / brotli negotiation + streaming
│   ├── cors.rs          CORS + preflight (bypasses auth)
│   ├── headers.rs       custom response headers
│   ├── ip_filter.rs     CIDR allow/deny, X-Forwarded-For
│   ├── limits.rs        maxBodyBytes (413), maxHeaderBytes (431)
│   ├── logging.rs *     5 log formats, atomic file switching
│   ├── rate_limit.rs *  token-bucket, in-memory or Redis
│   ├── rate_limit_redis.rs  Redis fixed-window counter with fallback
│   ├── redirects.rs     path redirects with :param captures
│   ├── response_time.rs * X-Response-Time header
│   └── security_headers.rs  HSTS, CSP, X-Frame-Options, etc.
│   (Rhai scripting middleware moved to crates/conduit-script-rhai, WASM
│    plugin middleware to crates/conduit-plugin-wasm, and the
│    MiddlewareGuard/MiddlewareResponseFilter dispatch to
│    crates/conduit-middleware — issue #114/#141; formerly filter/script.rs
│    and filter/wasm.rs here)
├── upload/
│   └── server.rs        upload server (Axum loopback, port 0)
└── util/
    ├── log_writer.rs    atomic log file writer (Arc<Mutex<Inner>>)
    ├── mime.rs          Content-Type detection
    ├── path.rs          path utilities
    └── net.rs           network utilities
```

---

## Cargo Workspace Crate Extraction Recipe

Conduit 2.0 (issue [#114](https://github.com/lopatnov/conduit/issues/114)) moves each
Cargo feature into its own workspace member crate under `crates/`. Every extraction —
`conduit-otlp`, `conduit-acme`, `conduit-auth-jwt`, ... — follows the same recipe, derived
from how `conduit-core` ([#126](https://github.com/lopatnov/conduit/issues/126)) and
`conduit-config-core` ([#127](https://github.com/lopatnov/conduit/issues/127)) were
actually built and independently audited. Read this before extracting a new crate,
whether by hand or via the `crate-extractor` agent.

### The five rules

1. **Re-export at the original location.** Every relocated item gets a `pub use` at its
   original file (and, where practical, its original line) in the root crate — e.g.
   `crate::config::schema::CONFIG_VERSION` still resolves — `src/config/schema/mod.rs` (formerly
   `schema.rs`, split into a directory in #314) has it as
   `pub use conduit_config_core::parse::CONFIG_VERSION;`. This is what keeps
   `conduit::`-prefixed paths — and therefore every existing integration test — compiling
   unchanged. Never do one blanket top-level re-export (`pub use conduit_x as x;` in
   `lib.rs`) — if the root already has a real module at that name (e.g. `conduit::config`
   holding `AppConfig`/`SiteConfig`), a blanket re-export conflicts with it instead of
   extending it.

2. **Generic-in-crate, bound-by-type-alias-in-root.** If the extracted item became generic
   over the config payload type (because the member crate can't know about `AppConfig`),
   root binds it: `pub type X = crate_x::X<AppConfig>;` plus a constructor that injects any
   root-only policy (a validator closure, etc.). See `Provider<C>`/`FileProvider<C>` in
   `crates/README.md` for the worked example. Path preserved; signature intentionally
   changed — record that as a deliberate break, don't pretend it's transparent.

3. **Schema-bound wrapper, same name and signature.** If the extracted item is generic
   *and* root's version does extra schema-specific work on top, keep a wrapper in root with
   the **identical pre-migration name and signature** that calls the generic version and
   then does the schema step — e.g. `src/config/parse.rs`'s `load_config`/`from_str`/
   `from_yaml` call `conduit_config_core::parse::{load_file, from_json_str, from_yaml_str}`
   and then `normalize()` into `AppConfig`.

4. **Anything not re-exported must be `pub(crate)`, not `pub`.** A member crate's `pub` API
   is a semver commitment once these crates start publishing to crates.io (see the
   `crates.io publishing` risk in [#114](https://github.com/lopatnov/conduit/issues/114)).
   Before merging an extraction, grep the new crate for `pub fn`/`pub struct`/`pub enum`
   and confirm each either has a re-export site in root or is genuinely meant to be public
   API — don't leave something `pub` just because it compiles. (This rule exists because
   `conduit_core::filter::path::path_matches` was accidentally hoisted from `pub(crate)` to
   `pub` during the `conduit-core` extraction and caught only in a later audit — see
   `crates/conduit-core/src/filter/path.rs`.)

5. **Extracting the aggregate itself (`AppConfig`/`SiteConfig`, #222) is all-or-nothing.** A
   struct's fields must name types from crates it depends on, so every type a `SiteConfig`
   field points at has to move with it (or already live in a crate it can depend on) — it cannot
   be cut into slices the way a leaf can. Do the module split as a separate earlier step
   (#314), then move the whole directory with `git mv`. Rule 1's "original location" for an
   aggregate is a *module path*: a facade whose body is an explicit `pub use conduit_config::schema::{…}`
   (not a glob) satisfies it, and keeps every existing `crate::config::schema::X` call site unedited.

### Two things that are *not* part of the recipe (deliberately)

- **`conduit-core` dependency is opt-in, not automatic.** Only depend on
  `lopatnov-conduit-core` if the new crate implements a chain trait (`RequestFilter`,
  `ResponseFilter`) and therefore needs `&mut Session`/pingora types. A crate with no
  request-lifecycle behavior (like `conduit-otlp`'s tracer init) takes primitives
  (`&str`, `u16`, ...) instead and has no pingora dependency at all.
- **Chain assembly and ordering live in `conduit-runtime`, never in a feature crate.**
  `crates/conduit-runtime/src/filter/chain.rs` decides guard order (see `CLAUDE.md`
  decision #20; `src/filter/chain.rs` in the root is a facade); a feature crate exports a
  filter implementation and a constructor, never a chain position.

### Watch for name collisions during extraction

Two functions can share a name and *look* like duplication candidates without being
duplicates — e.g. `conduit_core::filter::path::path_matches` (exact-only fallback) vs.
`src/proxy/cache.rs`'s private `path_matches` (prefix-matches even without `/**`). Check
behavior, not just the signature, before "deduplicating" anything found this way.

### A config struct's implementation backends can live in their own crates

`conduit-middleware` (issue #114/#141) is the first extraction where a config struct
(`MiddlewareEntry`) and its feature-gated implementation backends land in **three**
different crates rather than one: `conduit-middleware` owns `MiddlewareEntry` plus the
dispatcher (`MiddlewareGuard`/`MiddlewareResponseFilter`, moved verbatim — still a closed
`match` on `entry.r#type`, not a new plugin trait/registry), while the two backends it
dispatches to (`run_script`/`run_script_response` for Rhai, `run_wasm`/`run_wasm_response`
for WASM) live in their own sibling crates (`conduit-script-rhai`, `conduit-plugin-wasm`)
and are pulled in as `conduit-middleware`'s *own* optional path-dependencies, gated behind
its own `rhai`/`wasm` Cargo features. The root crate's `rhai`/`wasm` features simply
forward into `conduit-middleware`'s features — this is the only crate that depends on
either backend crate directly. Worth this shape specifically when a dispatcher's backends
are large/independent enough to be their own crates (bringing their own dependency trees —
`wasmtime`, `rhai` — that nothing else in the workspace needs) but the dispatcher itself
still needs to be always-compiled for the same config-parses-everywhere reason every other
`MiddlewareEntry`-shaped struct is (`CLAUDE.md` decision-#20a-style `feature_warnings()`).

### A feature crate owns its config validation and its feature-off warning

The crate that owns a config block also owns what `config::validate` says about it (issue
[#316](https://github.com/lopatnov/conduit/issues/316)). Up to two always-compiled modules, next to the config types:

- **`src/validate.rs`** — the block's validator, `pub fn validate_x(cfg, prefix, errors)`, reporting through
  `conduit_config_core::validation::ValidationError` (so the crate depends on `lopatnov-conduit-config-core`).
  The root's `src/config/validate/site.rs` calls it. A check that needs the whole `SiteConfig`/`AppConfig` (a
  combination of two blocks, proxy-loop detection, the Redis cross-site check) stays in the root: the
  `layer-boundaries` CI job rejects those types in a member crate.
- **`src/warnings.rs`** — `pub const COMPILED: bool = cfg!(feature = "<this crate's feature>")` and
  `pub fn feature_warning(i, cfg) -> Option<String>`, which is `None` when the feature is compiled in or the block is
  absent. The message text lives here, not in the root. A test that needs the whole site (`redis`, `cache`) is
  evaluated in the root and handed over as a `bool`. Add a text-pin test for the message.

The root then needs exactly two lines per feature in `src/config/validate/warnings.rs`: a flat call in
`check_site_simple_feature_warnings` (its position is the position of the warning in `feature_warnings()`'s output),
and a `const _: () = assert!(<crate>::warnings::COMPILED == cfg!(feature = "<root feature>"), ..)` next to the others.
The assert is what keeps the two features in step: if a crate feature is ever enabled without the root feature (or the
reverse), the build fails instead of the warning silently disappearing. Cargo features are unified per build, so it
cannot be checked from the crate alone.

The golden tests (`src/config/validate/golden_tests.rs`, fixtures in `testdata/`) pin the exact ordered output of
`validate()` and `feature_warnings()` in every feature combination; a new feature-off warning has to be added to the
fixture, and the baseline is regenerated only for a deliberate behaviour change (see that file's module comment).

---

## Adding a New Feature

> Written 2026-10-03, closing issue [#259](https://github.com/lopatnov/conduit/issues/259).
> The extraction recipe above covers moving **already-existing** code into its own crate.
> This covers the other direction: designing and wiring up a genuinely **new**,
> independently-compilable feature from scratch, in the shape the 2.0 workspace (#114,
> merged into `main`) settled on. Derived from how real optional features — `acme`,
> `fault-injection`, `tokio-metrics` — are actually built, not guessed upfront (per the
> issue's own owner decision to wait until the migration's patterns had solidified).

A new optional feature in this workspace is a new crate, a Cargo feature on it, a
forwarding feature on the root crate, a config schema entry, a feature-off warning, and
tests/docs — in that order. Skipping a step produces exactly the kind of silent gap
`integrity-auditor` exists to catch later (see `.claude/rules/best-practices.md` §1), so
do them all up front.

### 1. New crate under `crates/`

`cargo new --lib crates/conduit-<name>`, then shape its `Cargo.toml` like
`crates/conduit-acme/Cargo.toml`:

- Package name `lopatnov-conduit-<name>`, lib name `conduit_<name>` (the `lopatnov-`
  prefix is the crates.io publishing name — `CLAUDE.md` decision #32 — the lib name is
  what code actually imports).
- If the feature's config struct should always parse (so `conduit validate` still gives a
  precise error when the feature is compiled out — the config-always-parses invariant
  every `SiteConfig`/`AppConfig` field relies on), keep the config type's own dependencies
  (`serde`) mandatory and put everything the *real implementation* needs (the guard/filter,
  any async runtime, any third-party client) behind `optional = true` so the crate compiles
  with zero extra dependencies when the feature is off.
- One Cargo feature on the new crate, named after the feature, listing every `dep:x` it
  turns on (see `conduit-acme`'s `[features] acme = [...]`).

### 2. Root crate wiring

In the root `Cargo.toml`:

- Add `lopatnov-conduit-<name> = { path = "crates/conduit-<name>", version = "2.0.0" }` to
  `[workspace.dependencies]` (keep the version string in lockstep — see
  `conventions.md`'s "Versioning"; `scripts/check-workspace-versions.sh` catches drift).
- Add `lopatnov-conduit-<name>.workspace = true` under `[dependencies]` — mandatory if the
  config struct always parses, `optional = true` if the whole crate is feature-gated.
- Add a forwarding feature: `<name> = ["lopatnov-conduit-<name>/<name>", ...]`, plus
  `"lopatnov-conduit-runtime/<name>"` and/or `"lopatnov-conduit-server/<name>"` for each
  downstream crate that has its own `#[cfg(feature = "<name>")]`-gated code for it — check
  what the feature actually needs rather than assuming one fixed shape: `otlp` and
  `tokio-metrics` forward into `runtime` (per-request span code, the lag gauge), `tcp`
  forwards only into `server` (no runtime/chain involvement), and `acme`/`tokio-metrics`
  forward into both (see those entries in the root `[features]` table).
- Decide whether it belongs in a bundle (`standard`, `gateway`, `full` — see the
  `[features]` table's "Convenience bundles" comment) — a niche or heavyweight feature
  (chaos testing, a scripting/WASM engine) stays out of `standard`/`gateway` and only goes
  in `full`.
- If the feature gates an integration test file, add `[[test]] name = "<name>"` +
  `required-features = ["<name>"]` so `cargo test` without the feature doesn't even try
  to compile it (see the `acme`/`cache` entries at the bottom of `Cargo.toml`).

### 3. Config schema entry

- Add the config struct's field to whichever `SiteConfig`/`AppConfig` (or nested struct)
  it belongs under, in the owning crate if it's a leaf (not the aggregate — see Rule 5 of
  the extraction recipe above for why the aggregate itself can't be touched piecemeal).
- Update **`schema/conduit.schema.json`** by hand to match — it's hand-maintained, not
  generated (see `CLAUDE.md` "Правила"). `scripts/check_schema_superset.py` (CI job
  `schema-superset-check`) best-effort-checks named structs against it; validate the JSON
  itself with `node -e "JSON.parse(fs.readFileSync('schema/conduit.schema.json','utf8'))"`.

### 4. Feature-off warning (`feature_warnings()`)

Every feature that can appear in a config but be compiled out needs its presence to
produce a warning, not a silent no-op, when the build doesn't have it — this is the
"config always parses" promise working end-to-end. Two files in the new crate (see
`crates/conduit-acme/src/warnings.rs` for the worked example):

- `src/validate.rs` — `pub fn validate_<name>(cfg, prefix, errors)`, reporting through
  `conduit_config_core::validation::ValidationError`. **Call it from the per-site
  validation in `crates/conduit-server/src/config/validate/site.rs`** (see how
  `validate_tcp`/`validate_ip_filter` are already wired in there) — a validator that's
  never called compiles cleanly and is dead code: invalid configs pass `conduit validate`
  with no error.
- `src/warnings.rs` — `pub const COMPILED: bool = cfg!(feature = "<name>")` and
  `pub fn feature_warning(i, cfg: Option<&YourConfig>) -> Option<String>`, `None` when
  compiled in or the block is absent. Pin the exact message text with a test.

Then wire both into the root (now `crates/conduit-server/src/config/validate/`, facaded
at `src/config/validate/mod.rs`):

- One call in `check_site_simple_feature_warnings` (`crates/conduit-server/src/config/
  validate/warnings.rs`) — its position in that function is the position of the warning
  in `feature_warnings()`'s output, so place it where it reads naturally alongside its
  siblings.
- One compile-time assert in the **root** crate's `src/config/validate/mod.rs` (not the
  `conduit-server` copy — see that file's own comment on why the split happened):
  `const _: () = assert!(conduit_<name>::warnings::COMPILED == cfg!(feature = "<name>"), "...")`.
  This is what fails the build if the crate's own feature and the root's forwarding
  feature are ever enabled independently of each other — Cargo features are unified per
  build, so this can only be checked from the root, never from the leaf crate alone.
- If the config key can land in `SiteConfig.extra` when the feature is off, add it to
  `DISABLED_KEY_OWNING_FEATURE` in `crates/conduit-server/src/config/validate/warnings.rs`
  so the generic "unknown key" warning becomes a specific "recompile with --features x" one.
- Update the golden tests (`src/config/validate/golden_tests.rs`, fixtures in `testdata/`)
  — a new feature-off warning changes `feature_warnings()`'s pinned output in every feature
  combination that doesn't have the feature on.

### 5. Docs

- `docs/configuration.md` — the new config block, with an example.
- `README.md`'s feature table, `crates/README.md`'s crate inventory, `docs/building.md` if
  it changes a bundle's contents — grep every changed feature name across all of these by
  content, not by an expected keyword (a past PR's `--features`/"standard" grep missed a
  plain feature table that used neither word — see `.claude/commands/
  feature-workspace-cycle.md` Step 6's note).
- `CLAUDE.md`'s "Беклог" if this closes a tracked backlog item, with the implementation's
  actual shape (module path, config field names) — not just a checkbox flip.

### 6. Tests

See the `testing` skill for conduit's actual mocking idioms. At minimum: a unit test for
the config struct's `Default`/deserialization, a validator test (both a valid and an
invalid config), a `feature_warning` text-pin test, and — if the feature has real runtime
behavior — an integration test gated by `required-features` as set up in step 2.

### Verification

Same bar as any PR (`conventions.md`'s PR checklist): `/build` on both default and
`--features full`, `feature-matrix-runner` (mandatory — this is exactly the kind of PR
that touches `[features]`), and `security-engineer` sign-off before merge if the feature
touches auth/secrets/TLS/rate-limit/CORS/the guard chain.

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

### Verifying a refactor or an extraction

`cargo test` does not show a feature that silently stopped being compiled or a test that
silently stopped running. For a PR that moves code between crates or changes the feature graph,
run the whole chain once on the final head:

```bash
scripts/verify-local.sh --base <rev-before-the-change>      # ~1 h; --quick skips tests, goldens and cargo hack
scripts/verify-local.sh --moved-to lopatnov-conduit-config:config:: --also-pkg lopatnov-conduit-config
```

It checks CI's dependency-leak rule, that the set of third-party crates and the list of tests are
unchanged against the base (tests that moved into another package must reappear there), clippy
`-D warnings` on eight profiles, the tests, the validation golden tests in every feature set, and
`cargo hack --each-feature`. See the header of the script for the options; it writes
`target/verify-local/summary.txt` and exits non-zero on any FAIL. Do not commit while its
`cargo hack` step runs (cargo-hack rewrites the manifests until it exits).

### Reading everything said on a pull request

```bash
scripts/pr-comments.sh <pr> [--since 2026-09-26T19:00:00Z] [--full]
```

Prints the PR's head SHA and state, every check that is not green (a bot whose check is still
pending has not commented yet), and all three comment streams from every author — issue comments,
reviews and inline review comments. The PR page folds resolved and outdated threads and a green
check list says nothing about comments, so use this before merging (a review a bot posts on a
later commit is easy to miss otherwise).

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
| [Cloudflare Pingora 0.9](https://github.com/cloudflare/pingora) | Async HTTP proxy framework |
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
