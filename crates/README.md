# crates/

Feature crates for the Conduit 2.0 workspace migration (see [#114](https://github.com/lopatnov/conduit/issues/114))
land here — one crate per Cargo feature, extracted incrementally per the phase
plan in that issue.

Each crate is named `lopatnov-conduit-<name>` and inherits shared metadata
(`version`, `edition`, `license`, `repository`) from `[workspace.package]` in
the root `Cargo.toml` via `<field>.workspace = true`.

## Members

- **`conduit-core`** (Phase 2.1, [#126](https://github.com/lopatnov/conduit/issues/126))
  — Layer-0 vocabulary: traits, outcome enums, and narrow context types with
  zero config knowledge (`RequestFilter`/`ResponseFilter`, `LocalHandlerImpl`,
  `is_path_skipped`, `AcceptEncoding`, `LogWriter`). Compiled into every
  build regardless of feature selection — the root crate's `src/`
  re-exports these through thin facades; concrete guards/filters and
  config-aware chain assembly stay in the root crate. **`util::mime`**
  (`content_type`, mime_guess-based) originally lived here too but was
  removed entirely and absorbed into `conduit-static` (#114/#139) — its
  only caller was the static-file handler that moved there, and leaving it
  behind would have kept `mime_guess` in the dependency tree unconditionally
  regardless of that crate's own `static` feature gate.

- **`conduit-config-core`** (Phase 2.2, [#127](https://github.com/lopatnov/conduit/issues/127))
  — Layer-0 config-loading mechanism, generic over the config payload type:
  env-var interpolation, JSON/YAML format detection, version-gated parsing
  (`parse::load_file<T>`), `ValidationError`, and the `Provider<C>`/
  `FileProvider<C>` file-watching abstraction with an injected validator
  closure. **`Provider<C>` is a deliberate 2.0 API break** — pre-migration
  it was a non-generic `Provider` trait; any external `impl Provider for X`
  now needs `impl Provider<AppConfig> for X`. Root's `src/config/provider.rs`
  keeps the ergonomic pre-migration name via `pub type FileProvider =
  conduit_config_core::provider::FileProvider<AppConfig>;` plus a
  constructor injecting the real validator — this generic-in-crate,
  bound-by-type-alias-in-root pattern is the recipe for any future
  extraction whose Layer-0 piece needs to stay schema-free while root wants
  an ergonomic, schema-bound name (see `CONTRIBUTING.md`'s crate-extraction
  recipe). Also compiled into every build. `ConfigFile`/`normalize()` and
  `src/config/defaults.rs` deliberately stay in the root crate — the former
  is a schema decision coupled to `AppConfig`/`SiteConfig` (moves with them
  in Phase 3), the latter is mostly dead/per-feature policy, not a Layer-0
  concern. Since [#316](https://github.com/lopatnov/conduit/issues/316) its
  `ValidationError`/`Severity` are also what the `validate.rs` module of each
  feature crate reports through (see `CONTRIBUTING.md`, "A feature crate owns
  its config validation and its feature-off warning").

- **`conduit-config`** (Phase 5.7, [#222](https://github.com/lopatnov/conduit/issues/222))
  — Layer-2 config schema: `AppConfig`, `SiteConfig` and every type they
  contain (the root-owned ones — `GlobalConfig`, `TlsConfig`, `LoggingConfig`,
  `ApiKeyConfig`, ... — are defined here; the rest are re-exported from the
  Layer-1 crate that owns the feature), plus `ConfigFile`/`normalize()` and
  `load_config`/`from_str`/`from_yaml`, which bind `conduit-config-core`'s
  generic loader to that schema. It depends on every Layer-1 config crate
  (always, never optionally: every field stays parseable in every build) and
  has **no Cargo features** — relocating the schema does not gate any field.
  Root's `src/config/schema/mod.rs` (an explicit re-export list) and
  `src/config/parse.rs` are facades over it, so every `crate::config::…` path
  keeps resolving. What deliberately stays in the root crate: `validate`
  (`validate()`/`feature_warnings()`, whose 17 feature-parity asserts compare a
  crate's `COMPILED` with the *root's* feature), the file/Kubernetes
  providers, `defaults.rs` and `rate_limit_scan.rs`.

- **`conduit-otlp`** (Phase 3.1, [#129](https://github.com/lopatnov/conduit/issues/129))
  — the template extraction for every subsequent feature crate. Owns
  `OtlpConfig` (the `global.otlp` config struct) and the OTLP tracer-provider
  lifecycle (`tracer::init_tracer`/`tracer::shutdown_tracer`). Compiled into
  every build like `conduit-core`/`conduit-config-core` above — not gated
  behind `optional = true` — because `GlobalConfig.otlp` is not itself
  feature-gated: a config that sets `global.otlp` without `--features otlp`
  must still parse cleanly and get an explicit `feature_warnings()` warning,
  not silently vanish. Only the real exporter wiring is gated, behind this
  crate's *own* `otlp` Cargo feature (mirroring the pre-extraction
  `src/server/otel.rs`'s internal `#[cfg(feature = "otlp")]` stub/impl split);
  the root crate's `otlp` feature forwards into it via
  `lopatnov-conduit-otlp/otlp`. Per-request span creation/finishing
  (`RequestCtx.otel_span`, `src/proxy/request_phase.rs`,
  `src/proxy/logging_phase.rs`) deliberately stays in the root crate — see
  `CLAUDE.md`'s architectural decision #30 — since it needs
  `pingora_proxy::Session`/`RequestCtx`, which this crate has no dependency
  on at all (matches `CONTRIBUTING.md`'s "conduit-core dependency is opt-in,
  not automatic" note).

- **`conduit-acme`** (Phase 3.2, [#130](https://github.com/lopatnov/conduit/issues/130))
  — ACME (Let's Encrypt) auto-TLS. Owns `AcmeConfig` (the `tls.acme` config
  struct, `src/config.rs`), the HTTP-01 certificate flow (`flow` — account
  management, order/challenge negotiation, renewal), and the HTTP-01
  challenge-response handler (`challenge`). `AcmeConfig` is compiled into
  every build like `conduit-otlp`'s `OtlpConfig` above — `TlsConfig.acme`
  isn't itself feature-gated, so it must stay parseable (and warn via
  `feature_warnings()`) without `--features acme`. Unlike `conduit-otlp`,
  though, `flow`/`challenge` have **no unconditional counterpart** at
  all — the pre-extraction `src/server/acme.rs` and
  `src/handler/acme_challenge.rs` were both already whole-file
  `#![cfg(feature = "acme")]`, so both modules are declared behind this
  crate's own `acme` feature in `lib.rs` rather than getting a no-op stub;
  the root crate's own module declarations (`src/server/mod.rs`,
  `src/handler/mod.rs`) already gate inclusion of the facade files the same
  way. `challenge` is this workspace's first feature crate to depend on
  `lopatnov-conduit-core` — it implements `LocalHandlerImpl`, which needs
  `&mut pingora_proxy::Session` (see `CONTRIBUTING.md`'s "conduit-core
  dependency is opt-in, not automatic": opt in when a chain/handler trait is
  implemented, skip it otherwise). `AppState.acme_challenges:
  Arc<DashMap<String, String>>` (`src/proxy/service.rs`) deliberately stays
  in the root crate — it's a plain third-party type shared with
  `RedirectProxy` (`src/server/redirect.rs`, always compiled), not an
  ACME-specific type to extract. `instant-acme`/`rcgen` are now path-dep-only
  behind this crate's `acme` feature; `rcgen` remains an unconditional root
  `[dev-dependencies]` entry for in-memory TLS test certificates, unrelated
  to ACME.

- **`conduit-faults`** (Phase 3.4, [#132](https://github.com/lopatnov/conduit/issues/132))
  — fault injection (chaos testing). Owns `FaultInjectionConfig`/`FaultAbort`/
  `FaultDelay` (the `sites[].faultInjection` config structs) and the real
  `guard::FaultInjectionGuard` — a request guard that aborts or delays a
  configurable percentage of requests. `FaultInjectionConfig` is compiled
  into every build like `conduit-otlp`'s `OtlpConfig` above —
  `SiteConfig.fault_injection` isn't itself feature-gated, so it must stay
  parseable (and warn via `feature_warnings()`) without `--features
  fault-injection`. Only the real `guard::FaultInjectionGuard` is gated
  behind this crate's own `fault-injection` Cargo feature; the root crate's
  `fault-injection` feature forwards into it via
  `lopatnov-conduit-faults/fault-injection`. This is #114's deliberately
  "smallest guard-shaped extraction" — `FaultInjectionGuard` implements
  `conduit-core`'s `RequestFilter` chain trait directly (the same trait
  every other in-chain guard implements, unlike the handler/service-shaped
  `conduit-otlp`/`conduit-acme` extractions above), so this crate depends on
  `lopatnov-conduit-core` (see `CONTRIBUTING.md`'s "conduit-core dependency
  is opt-in, not automatic" — `conduit-acme`'s `challenge` module was the
  first crate to take this dependency; this is the second). Chain assembly
  and guard ordering stay in the root crate's `src/filter/chain.rs`
  (`CLAUDE.md` decision #20).

- **`conduit-cache`** (Phase 3.7, [#135](https://github.com/lopatnov/conduit/issues/135))
  — HTTP response caching. Owns `CacheConfig` (the `proxy.*.cache` config
  struct, `src/config.rs`), the always-compiled cache-key/policy logic
  (`cache` module — `build_cache_key`, `should_cache_request`,
  `response_cacheable`, `cache_storage`, `cache_lock`), the disk storage
  backend (`disk` module), the Redis storage backend (`redis` module, gated
  behind this crate's own `redis` feature), and `ctx::CacheReqState` — the
  per-request cache state struct. `CacheConfig` is compiled into every build
  like `conduit-otlp`'s `OtlpConfig` above. Unlike the guard-shaped
  extractions above, most of this crate's own code is *also* always
  compiled — not gated behind `optional = true` internal `#[cfg]`s beyond
  what it already had pre-extraction: Pingora's `ProxyHttp` trait calls
  `cache_key_callback`/`response_cache_filter` on every request regardless
  of the `cache` feature, and the Admin API's cache-purge handler calls
  `cache::build_cache_key`/`cache::cache_storage` unconditionally too — so
  `src/proxy/cache.rs`/`cache_disk.rs` had no file-level feature gate before
  this move, and neither do their new homes (`cache`/`disk` modules here).
  Only `cache::should_early_refresh` and the entire `redis` module are
  genuinely gated, via this crate's own `cache`/`redis` Cargo features
  (`redis` forwarded from the root as a plain, not weak, dependency
  feature — see that crate's `src/lib.rs` doc comment for why the `?/`
  syntax doesn't apply to a mandatory dependency). Has no dependency on
  `lopatnov-conduit-core` — nothing here implements `RequestFilter`/
  `ResponseFilter`; the Pingora `ProxyHttp` trait-method bodies stay in the
  root crate's `request_phase.rs`/`response_phase.rs`, calling into this
  crate's plain functions. Per-request `cache_age_secs`/
  `early_refresh_upstream_url` state moved into `ctx::CacheReqState`, held
  behind a `#[cfg(feature = "cache")]`-gated `RequestCtx.cache` field in the
  root crate (`CLAUDE.md` decision #30) — same pattern as
  `conduit-auth-jwt`'s `JwtReqState`/`RequestCtx.jwt`.

- **`conduit-ipfilter`**, **`conduit-cors`**, **`conduit-security-headers`**
  (Phase 3.8, [#136](https://github.com/lopatnov/conduit/issues/136)) —
  three small, mutually-independent guard extractions batched into one PR
  (same "small independent leaves" batching used for #131/#135). Own
  `IpFilterConfig`; `CorsConfig`/`CorsOptions`; and
  `SecurityHeadersConfig`/`SecurityHeadersOptions` respectively (the
  `sites[].ipFilter`/`cors`/`securityHeaders` config structs), plus the pure
  header/matching logic (`ip_filter`/`cors`/`security_headers` modules) and
  the real chain guards (`guard::IpGuard`/`guard::CorsPreflight`/
  `guard::AllowedHostsGuard`). **Unlike every extraction above, none of the
  three is gated behind a Cargo feature at all** — per `CLAUDE.md`
  architectural decision #31 (2026-08-23), `ipFilter`/`cors`/
  `securityHeaders` stay always-on/default-on: gating them would buy almost
  no binary-size benefit (light logic, no heavy third-party dependency)
  while adding a real "forgot the flag, silently stopped filtering" risk for
  security-relevant guards. Each crate has **no `[features]` table**, and
  every dependency — including `lopatnov-conduit-core` for the real guard —
  is mandatory, not `optional = true`; this mirrors `conduit-config-core`'s
  (#127) unconditional dependency style rather than the config-always-on/
  guard-feature-gated split used by every extraction above. All three guards
  implement `conduit-core`'s `RequestFilter` chain trait directly (see
  `CONTRIBUTING.md`'s "conduit-core dependency is opt-in, not automatic").
  Chain assembly and guard ordering stay in the root crate's
  `src/filter/chain.rs` (`CLAUDE.md` decision #20).

- **`conduit-ratelimit`** — **slices 1-2** of
  [#137](https://github.com/lopatnov/conduit/issues/137) (`conduit-limits` —
  a separate, unrelated config type despite the similar name — is still
  open; this covers rate limiting only, not the whole issue). Owns
  `RateLimitConfig` (the shared `rateLimit` shape used at site/route/
  consumer level), the pure token-bucket admission logic (`bucket` module:
  `TokenBucket`, `RateLimiter`, `MAX_BUCKETS`, `cleanup`, `check_key`/
  `check_key_for` — the single capacity-checked admission point every
  rate-limit layer shares), and, behind this crate's own `redis` feature
  (slice 2), the Redis-backed limiter (`redis` module — a fixed-window
  counter, a real algorithm difference from the always-on token bucket).
  **Not yet moved**: the `Session`-aware `extract_key`/`check` wrappers
  (`src/filter/rate_limit.rs`), which stay in the root crate for the same
  reason `IpGuard`/`CorsPreflight` do. The always-on part has no `[features]`
  table entry of its own (same always-on rationale as `conduit-ipfilter`/
  `conduit-cors`/`conduit-security-headers` above); `redis` mirrors
  `conduit-cache`'s always-on-base + optional-`redis` shape. Slice 1 was
  created to resolve a SonarCloud "Duplicated Lines on New Code" finding:
  `conduit-auth-consumers` had carried a deliberate, documented temporary
  duplicate of `RateLimitConfig` since #134 (a Layer-1 crate couldn't depend
  on a type living in the root crate that depends on *it*); both the root
  crate's and `conduit-auth-consumers`'s copies now re-export this crate's
  single type instead. Slice 2 site-scoped the Redis backend's key
  construction (issue #317, the Redis-backend twin of #303/#304's in-memory
  fix) while moving it.

- **`conduit-limits`** — the second (final) half of
  [#137](https://github.com/lopatnov/conduit/issues/137) (`conduit-ratelimit`
  above covers rate limiting; this covers the separate, similarly-named
  `LimitsConfig`). Owns `LimitsConfig` (the `sites[].limits` config struct),
  the pure limit-checking logic (`limits` module — declared-Content-Length /
  header-size checks and the leaky-bucket minimum-upload-rate algorithm from
  issue #51), the real `guard::LimitsGuard` chain guard (Host-header
  validation, `maxRequestHeaders`, `maxInflightRequests`, body/header size
  limits, and `maxConnectionsPerIp` via the RAII `guard::IpConnSlotGuard`),
  and `ctx::LimitsReqState` — the per-request state `RequestCtx` threads
  through the request-body pipeline. **No `[features]` table** — same
  always-on rationale as `conduit-ipfilter`/`conduit-cors`/
  `conduit-security-headers`/`conduit-ratelimit`'s slice 1 above
  (`CLAUDE.md` decision #31); every dependency, including
  `lopatnov-conduit-core` for `guard::LimitsGuard`, is mandatory. Unlike
  `conduit_cache::CacheReqState`/`conduit_auth_jwt::guard::JwtReqState` (both
  `#[cfg(feature = "...")]`-gated on `RequestCtx` because `cache`/`jwt` are
  optional Cargo features), `RequestCtx.limits: conduit_limits::LimitsReqState`
  is a plain, always-present field — `limits` isn't optional, so there's no
  "feature not compiled in" state for it to represent. This extraction also
  closed [#51](https://github.com/lopatnov/conduit/issues/51)
  (`limits.minUploadRateBytesPerSec` slow-loris upload defense) as a side
  effect — that feature (config field, leaky-bucket algorithm, its 7 unit
  tests, and its `request_body_filter` wiring) was found already fully
  implemented pre-extraction and simply moved along with the rest of this
  crate's scope; no new code was needed to close it.

- **`conduit-compression`** (Phase 4.1, [#138](https://github.com/lopatnov/conduit/issues/138))
  — owns `CompressionConfig`/`CompressionOptions` (the `sites[].compression`
  bool-or-object-shorthand config, always compiled — same config-always-on
  rationale as `conduit-faults`/`conduit-auth-jwt`) and, behind this crate's
  own `compression` feature, the real negotiation logic in its `logic` module
  (`CompressOptions`, `effective`, `is_compressible_type`, `best_encoding`,
  `compress_bytes`). **The first extraction where the forwarding root-crate
  feature is default-on**, not default-off like every prior optional
  feature — issue #138 is explicit that response compression is a baseline
  expectation for a reverse proxy, so `default = ["compression"]`; only
  `--no-default-features` (or otherwise excluding `compression`) produces
  the "just static files, no compression" build the issue describes, and
  actually drops `async-compression` from the dependency tree. **Partial
  extraction, like `conduit-auth-consumers`'s guard**:
  `handler/static_files.rs`'s on-the-fly streaming compression
  (`stream_file_compressed`, the chunk-by-chunk brotli/gzip/deflate encoder
  pipeline) stayed in the root crate — out of #138's scope, which names only
  `src/filter/compression.rs` — but is gated behind
  `#[cfg(feature = "compression")]` directly there too, via the root crate's
  own direct (now `optional = true`) `async-compression` dependency, so
  disabling the feature drops the codec crates from the tree regardless of
  which crate's code references them. **Superseded by `conduit-static`
  (#114/#139, below)**: `handler/static_files.rs` itself (including
  `stream_file_compressed`) moved out of the root crate entirely, so the
  root crate's own direct `async-compression` dependency described above no
  longer exists — `conduit-static` carries that edge now, behind its own
  `compression` feature, forwarded from the root crate's `compression`
  feature alongside `conduit-compression`'s own forward.

- **`conduit-static`** (Phase 4.2, [#139](https://github.com/lopatnov/conduit/issues/139))
  — owns `StaticConfig`/`StaticOptions`/`FallbackConfig`/`FallbackRule` (the
  `sites[].static`/`staticOptions`/`fallback` config, always compiled — same
  config-always-on rationale as `conduit-faults`/`conduit-compression`) and,
  behind this crate's own `static` feature, the real serving logic:
  `handler` (`StaticFileHandler`/`handle_static`, moved from
  `src/handler/static_files.rs` in full — including its on-the-fly streaming
  compression, unlike `conduit-compression` which left that part behind),
  `fallback` (`FallbackHandler`/`handle_fallback`, moved from
  `src/handler/fallback.rs`), `roots` (`resolve_static_roots`, moved from
  `src/proxy/router.rs`), and `mime` (`content_type`, absorbed from
  `conduit-core`'s own `util::mime` — see that crate's entry above). **The
  second default-on extraction after `conduit-compression`** — issue #139 is
  explicit that static-file/fallback serving are baseline expectations for a
  reverse proxy/static-file server, so `default = ["compression", "static"]`;
  only `--no-default-features` produces a build with neither compiled in.
  This extraction also dropped `humantime`/`libc`/`mime_guess` as *direct*
  root-crate dependencies entirely (their only root-crate callers all moved
  into this crate) — each still reachable transitively via this crate's own
  gated edges, so `cargo tree -p lopatnov-conduit --no-default-features -i
  mime_guess` genuinely shows it absent, not just moved one hop sideways.

- **`conduit-hotreload`**, **`conduit-metrics`**, **`conduit-redirects`**
  (Phase 4.3, [#140](https://github.com/lopatnov/conduit/issues/140)) —
  three independent handler-shaped crates batched into one PR (same
  "small independent leaves" batching used for #131/#135/#136), each with a
  **different** feature-gating shape — the one genuine design decision this
  extraction had to make.
  - **`conduit-hotreload`** owns `HotReloadConfig`/`HotReloadOptions` (always
    compiled, same config-always-on rationale as `conduit-compression`/
    `conduit-static`) and, behind this crate's own `hotreload` feature, the
    real `handler` (`HotReloadJsHandler`/`HotReloadSseHandler`, serving
    `/__hot-reload__`/`/__hot-reload__/client.js`) and `watcher`
    (`build_watch_config`/`run_file_watcher`, the `notify`-backed file
    watcher). **Third default-on extraction** after `compression`/`static`
    — `default = ["compression", "static", "hotreload"]` — and, per
    `CLAUDE.md` decision #31, one of only two extracted features
    (`static` is the other) genuinely worth gating for real, since it pulls
    in `notify` and its platform-specific filesystem-watcher backend.
    `watcher::build_watch_config` needed a real signature change during the
    move, not just a relocation: the pre-extraction version iterated
    `AppConfig.sites` directly, but `AppConfig`/`SiteConfig` aren't
    extracted out of the root crate yet, so this crate can't name them —
    it now takes an iterator of `(Option<&HotReloadConfig>,
    Option<&conduit_static::StaticConfig>)` pairs instead, with the root
    crate's own caller (`admin/api.rs`) mapping `config.sites` into that
    shape. `router.rs::is_hot_reload_sse_path`/`is_hot_reload_js_path` and
    `request_phase.rs::build_handler`'s `HotReloadJs`/`HotReloadSse` arms
    were both gated behind `#[cfg(feature = "hotreload")]` as part of this
    move (issue #341's ACME-challenge bug class, applied proactively here
    rather than discovered as a gap afterward) — without the fix, disabling
    `hotreload` while a site configured `hotReload` would have made
    `/__hot-reload__`/`/__hot-reload__/client.js` requests fall through to
    Pingora's proxy path with no real upstream (a 502) instead of degrading
    to the site's own `fallback`/`static`/`proxy` config. **Caveat**:
    despite the feature gate, `notify` itself does not actually leave the
    overall dependency tree under `--no-default-features` — `conduit-config-
    core`'s unrelated, always-on config-file-reload watcher (`FileProvider`'s
    auto-reload mode, pre-existing since #127) has its own unconditional
    `notify` dependency. Gating this crate's own copy is still correct for
    feature-correctness; it just isn't a source of `notify` footprint
    savings by itself.
  - **`conduit-metrics`** owns `MetricsConfig` and the real
    `handler::MetricsHandler`/`handler::handle_metrics` (the Prometheus
    `/metrics` text-exposition endpoint) — **no top-level Cargo feature at
    all**, same always-on rationale as `conduit-cors`/`conduit-ipfilter`/
    `conduit-security-headers`/`conduit-redirects` (`CLAUDE.md` decision
    #31). `ConduitMetrics` (the metric-*registration* struct) deliberately
    stays in the root crate, destined for the future `conduit-runtime`; this
    crate's handler only *reads* the process-wide default registry via
    `prometheus::gather()`. Does have one independent `compression`
    sub-feature (mirrors `conduit-static`'s own), gating on-the-fly
    whole-body compression of the metrics response via
    `conduit_compression::logic::compress_small_body` (issue #338) —
    forwarded from the root crate's own default-on `compression` feature.
  - **`conduit-redirects`** owns `RedirectRule` and the real
    `guard::RedirectGuard` (configured URL redirects, 301/302/307/308) —
    also **no `[features]` table at all**, same always-on rationale.
    `guard::RedirectGuard` implements `conduit-core`'s `RequestFilter` chain
    trait directly (see `CONTRIBUTING.md`'s "conduit-core dependency is
    opt-in, not automatic"). Chain assembly stays in the root crate's
    `src/filter/chain.rs` (`CLAUDE.md` decision #20).

- **`conduit-script-rhai`**, **`conduit-plugin-wasm`**, **`conduit-middleware`**
  (Phase 4.4, [#141](https://github.com/lopatnov/conduit/issues/141)) — three
  crates landing together because `conduit-middleware` depends on the other
  two as its own optional path-dependencies (see `CONTRIBUTING.md`'s "A
  config struct's implementation backends can live in their own crates" for
  the general shape).
  - **`conduit-script-rhai`** owns the Rhai scripting backend
    (`run_script`/`run_script_response`, moved from `src/filter/script.rs`).
    **No `[features]` table at all** — whether it's compiled in is controlled
    entirely by `conduit-middleware`'s own `rhai` feature, not by anything
    declared here. No dependency on `lopatnov-conduit-core` either — it
    implements no chain trait, only plain functions taking/returning
    primitives and outcome enums. `ScriptRequest`/`ScriptResponse`/
    `ScriptResponseBuilder`/`ScriptUpstreamView` are `pub(crate)` — Rhai's
    `register_type_with_name`/`register_fn` need `'static + Clone + Send +
    Sync`, not `pub` visibility, and none has a caller outside this crate.
  - **`conduit-plugin-wasm`** owns the WASM plugin backend
    (`run_wasm`/`run_wasm_response`, moved from `src/filter/wasm.rs`, plugin
    ABI documented in this crate's own `src/lib.rs`). Same shape as
    `conduit-script-rhai` — no `[features]` table, no `conduit-core`
    dependency; `get_or_compile` (the WASM module cache lookup) demoted from
    `pub(crate)` to a private `fn` during the move (no caller outside its own
    module).
  - **`conduit-middleware`** owns `MiddlewareEntry` (the `sites[].middleware[]`
    config struct, `src/config.rs`, always compiled — same config-always-on
    rationale as `conduit-faults`/`conduit-otlp`) and the real dispatchers:
    `guard::MiddlewareGuard` (request phase, moved from `src/filter/chain.rs`)
    and `response::MiddlewareResponseFilter` (response phase, moved from
    `src/filter/response_chain.rs`). Both dispatch on `entry.r#type` with a
    closed `match` — deliberately **not** a `MiddlewarePlugin` trait/registry
    (issue #141's own original body described one that was never built; see
    issue #392 for that as a separate, later design question). Its own
    `rhai`/`wasm` Cargo features pull in `conduit-script-rhai`/
    `conduit-plugin-wasm` as optional path-dependencies and gate the
    corresponding dispatch arms — the root crate's `rhai`/`wasm` features
    simply forward into these. `tracing`/`serde_json`/`bytes`/`tokio` are
    mandatory dependencies here, not gated behind `any(rhai, wasm)`, because
    the "feature disabled: warn" arm in `guard::MiddlewareGuard::apply` is
    live precisely when `wasm` is OFF, and `MiddlewareEntry.config:
    Option<serde_json::Value>` is always compiled. `base64` is gated behind
    the `wasm` feature only (needed by the WASM response-body-override header
    hack in `response::apply_response_mutations` — a real, separately-filed
    pre-existing bug, issue #391, moved verbatim and not fixed as part of
    this extraction). Neither `conduit-script-rhai` nor `conduit-plugin-wasm`
    is a direct root-crate dependency anymore — `conduit-middleware` is the
    only crate that calls into either.

- **`conduit-k8s`** (Phase 4.5, [#249](https://github.com/lopatnov/conduit/issues/249))
  — the `KubernetesProvider`/`ConduitSite` CRD list+watch mechanism moved from
  `src/config/kubernetes.rs`. **No `[features]` table at all** — unlike most
  other optional-feature crates, every dependency here (`kube`, `k8s-openapi`,
  `schemars`, `futures`, plus the usual `tokio`/`async-trait`/`serde`/
  `serde_json`/`anyhow`/`tracing`) is a plain, non-optional dependency of this
  crate; the crate's own presence in the dependency graph — gated behind the
  root's `kubernetes` feature via `dep:lopatnov-conduit-k8s` — is what makes
  it optional, the same shape as `conduit-script-rhai`/`conduit-plugin-wasm`
  above.
  `KubernetesProvider<B>` and `build_app_config<B>` are generic over a new
  `CrdConfigBuilder` trait (`type Site`, `type Config`, `site_from_spec`,
  `build_config`) rather than naming `AppConfig`/`SiteConfig` directly — this
  crate can't know about conduit's real schema without creating a dependency
  cycle. Root's `src/config/kubernetes.rs` implements `CrdConfigBuilder` on a
  zero-sized `ConduitSchema` type and binds `pub type KubernetesProvider =
  conduit_k8s::KubernetesProvider<ConduitSchema>;` — the same "generic-in-
  crate, bound-by-type-alias-in-root" pattern as `conduit-config-core`'s own
  `Provider<C>`/`FileProvider<C>` above, just for a single bigger trait
  instead of a smaller generic parameter (the shape `conduit_upload`'s
  `UploadConfigSource` also follows). `build_app_config`/`spec_to_site_config`
  keep their pre-migration names and non-generic signatures in root via thin
  wrappers (recipe rule 3) rather than a direct `pub use` — the generic
  versions need an explicit `::<ConduitSchema>` turbofish that the original
  call sites (including this crate's own former unit tests) never had to
  supply.
  `PhantomData<fn() -> B>` (not bare `PhantomData<B>`) keeps
  `KubernetesProvider<B>` unconditionally `Send + Sync` regardless of `B` — a
  function pointer's phantom is always `Send + Sync`, so the struct's own
  auto-trait bounds don't accidentally depend on whatever concrete `B` the
  root crate binds.
  Test coverage split by what each half can test without creating a cycle:
  this crate's own tests cover the generic mechanism only (constructor/
  builder fields, `build_app_config`'s CRD-name error-attribution) via a
  trivial test-only schema binding; the tests exercising the *real*
  `SiteConfig`/`AppConfig` JSON round-trip stayed in root's
  `src/config/kubernetes.rs`, the only place that can construct the real
  schema types.

- **`conduit-upstream`** (Phase 5.1, [#142](https://github.com/lopatnov/conduit/issues/142))
  — owns `UpstreamRegistry` (per-upstream health state, inflight connection
  counts, the runtime upstream-override registry), Peak EWMA latency
  tracking, Outlier Detection, active health-check/connection-warmup
  background tasks, the half-open circuit breaker (`health` module, moved
  from `src/proxy/health.rs`), the `LoadBalancingStrategy` trait and all 8
  concrete strategy structs (`strategy` module, moved from
  `src/proxy/strategy.rs`), and URL parsing + the plain-slice load-balancing
  "pick" algorithms (`targets` module, most of the former
  `src/proxy/upstream.rs`). Also owns the LB/health config types:
  `LoadBalanceStrategy`, `ProxyTarget`/`WeightedTarget`, `UpstreamGroup`,
  `UpstreamHealthCheck`, `UpstreamTlsConfig`, `OutlierDetectionConfig`
  (`config` module). **One `proxy` feature (#144, PR 1), gating only the
  `reqwest`-backed connection warmup** — upstream selection/health tracking
  itself is not an optional Cargo feature (it stays always-compiled, matching
  `conduit-ipfilter`/`conduit-cors`/`conduit-metrics`'s always-on shape,
  `CLAUDE.md` decision #31, for a reason specific to this domain rather than
  that decision's own "light logic, no heavy dependency" rationale); the
  warmup's `reqwest` dependency is the one thing worth gating. Since #144
  PR 4a the root crate's own `proxy` feature forwards into this one (the
  unconditional pin is gone) and `admin/api.rs`'s warmup spawn is a
  two-variant function on that feature. No
  dependency on `lopatnov-conduit-core` either (see `CONTRIBUTING.md`'s
  "conduit-core dependency is opt-in, not automatic") — the Pingora
  `ProxyHttp` trait-method bodies stay in the root crate, calling into this
  crate's plain functions.
  **Partial extraction, deliberately not everything issue #142 named** (at
  the time — see below for how #143 later resolved this): `ProxyTarget`/
  `WeightedTarget` moved here (`UpstreamGroup`, also in scope, embeds
  `Vec<ProxyTarget>` directly — they had to travel together), but
  `ProxyConfig`/`ProxyRouteTarget` and three of the four functions that used
  to consume them (`target_urls`, `weighted_targets`, `target_urls_from_proxy`
  — the fourth, `strip_prefix_enabled`, turned out to be dead code, see
  below) stayed in the root crate's own `src/proxy/upstream.rs` at the time,
  right next to a facade re-export of everything that did move —
  `ProxyRouteTarget::Full` embeds `ProxyRouteConfig`, a large struct itself
  embedding `CacheConfig`/`RetryConfig`/`ConnectionPoolConfig`/
  `RateLimitConfig`/etc., none of which were extracted yet at #142's time —
  moving those functions here would have forced `ProxyRouteConfig` to move
  too, a genuine circular dependency with several other not-yet-extracted
  crates.
  **Resolved by `conduit-proxy-http` (issue #114/#143, Phase 5.2)**:
  `ProxyConfig`/`ProxyRouteTarget`/`ProxyRouteConfig` and the rest of proxy
  routing moved into that new crate, which now also owns `target_urls`/
  `weighted_targets`/`target_urls_from_proxy` (see its own entry below).
  `strip_prefix_enabled` did not move with them — it had zero real
  production call sites (only its own unit tests referenced it) and was
  deleted outright as dead code during #143's extraction rather than
  relocated.
  **The one real design decision, called out explicitly in issue #142's own
  text**: `health::spawn_health_checks`/`health::spawn_connection_warmup`
  used to take `&AppConfig` directly — for the same reason as the paragraph
  above, they now take an iterator of already-resolved
  `(&config::UpstreamHealthCheck, &[String])` pairs instead. The root
  crate's own call site (`admin/api.rs::health_check_routes`) resolves
  `AppConfig` down to that shape before calling in — the same "narrower
  slice instead of a root-only type" pattern `conduit-hotreload`'s
  `build_watch_config` already established for its own analogous problem
  (issue #114/#140), applied here to a second function pair in the same
  extraction rather than a new design.

- **`conduit-proxy-http`** (Phase 5.2, [#143](https://github.com/lopatnov/conduit/issues/143))
  — proxy target resolution: routing (`resolve`/`groups`/`routes_resolve`),
  candidate-pool building + peer/retry selection (`peer_pick`, `retry`),
  sticky-session resolution + HMAC helpers (`sticky`), `routes[]` array
  matching (`routes`), per-request state (`state`), the outcome boundary
  types (`outcome`), and per-upstream connection-capacity admission +
  the slow-start traffic ramp (`capacity`/`slow_start`, both private —
  nothing outside this crate's own routing code ever called them). PR B
  (final) of a 3-PR sequence: PR A1 (issue #418) grouped `RequestCtx`'s 14
  proxy-specific fields into `ProxyReqState`; PR A2 (issue #419) phase-split
  the root crate's `router.rs`/`routes.rs` and introduced the `ProxyOutcome`/
  `ProxyResolution`/`ProxyUpstream` boundary types (replacing
  `RouteResolution.upstream: UpstreamTarget` for the *inner* resolution
  functions, since `UpstreamTarget::Local(LocalHandler)` must stay root-crate
  vocabulary); this PR moved both PRs' work into the new crate.
  Also owns 10 proxy-related config types moved out of
  `src/config/schema.rs`: `ProxyConfig`/`ProxyRouteTarget`/`ProxyRouteConfig`/
  `StickyConfig`/`RewriteRule`/`ProxyTimeout`/`ConnectionPoolConfig`/
  `RetryConfig`/`RouteConfig`/`MatchConfig` (`config` module) — this is what
  let 3 of `conduit-upstream`'s own 4 deferred functions
  (`target_urls`/`weighted_targets`/`target_urls_from_proxy`, now in this
  crate's `targets` module) finally move out of the root crate, resolving
  the circular-dependency deferral #142 documented above. `strip_prefix_enabled`
  (the fourth) turned out to be dead code — no real production call site,
  only its own unit tests — and was deleted rather than moved.
  **`proxy` feature (#144, PR 1)** — gates the resolution engine
  (`resolve`/`groups`/`routes_resolve`/`peer_pick`/`retry`/`sticky`/
  `capacity`/`slow_start`, plus `hmac`/`sha2`/`base64`/`subtle`); config
  types, `state`, `outcome`, `options::ProxyCtx`, `targets` and the
  `routes` *matcher* stay always-compiled, because `routes[].static` needs
  `RouteConfig`/`MatchConfig` and the matcher even in a build with no
  proxying — so this crate can **never** become `optional = true` in the
  root crate. With the feature off, a matched `routes[]` entry that carries
  a `proxy` action resolves to `ProxyOutcome::Unresolved` (the site
  fallback), deliberately *not* to `NonProxy`: promoting a proxy-first
  route's dead `static` half to a live file root would expose a directory
  the operator never meant to serve. Since #144 PR 4a the root crate's
  own `proxy` feature forwards into this one (the unconditional pin is
  gone), so a root build without `proxy` really does not compile the
  resolution engine.
  **#144, PR 2:** the root crate got its own default-on `proxy` feature
  that gates the *router* (`sites[].proxy` is ignored, and `routes[]`
  proxy actions end in the site fallback, without it), plus
  `feature_warnings()`. In that build the router calls the always-compiled
  `routes::match_routes_unproxied` — same matching as `match_routes`, but
  no upstream is resolved and no counter/registry state or connection
  slot is touched (and, since PR 4a, the resolution engine itself is not
  compiled in that build). `route_limits_from_target` moved
  to the always-compiled `state` module so a never-proxied `routes[]`
  entry still carries its rate-limit/priority stamp (#360, #415).
  No dependency on `lopatnov-conduit-core`/pingora (nothing here implements
  `RequestFilter`/`ResponseFilter` — the `ProxyHttp` trait-method bodies
  stay in the root crate's `request/*.rs`/`response_phase.rs`, calling into
  this crate's plain functions).
  **`dispatch.rs` deliberately did NOT move here**, unlike its PR-A2
  siblings: it bundled `parse_rfc9218_priority` (pure string parsing) with
  site/local-path dispatch helpers (`find_site_idx`, `is_health_path`,
  `metrics_token`, `is_hot_reload_*`) that all take `&AppConfig`/
  `Option<&SiteConfig>` directly — still root-crate-only types (a separate,
  not-yet-started config-schema-decomposition track, issues
  #314/#315/#316/#222). Moving it would have created the exact circular
  dependency this crate's own `config` module exists to avoid. Relocated
  within the root crate instead (`src/proxy/routing/dispatch.rs` →
  `src/proxy/dispatch.rs`, content unchanged) — confirmed via grep before
  this decision that none of the files that DID move into this crate ever
  called any function in it.
  **`url_to_proxy_upstream` is a deliberate small duplicate**, not a shared
  call: this crate cannot call back into the root crate's own
  `router::url_to_proxy_upstream` (the dependency only goes one way), so its
  `outcome` module carries a private ~10-line copy with the same
  URL-parsing body, returning `ProxyUpstream` directly instead of
  `UpstreamTarget` — the two payloads are already byte-identical, so the
  duplicate is trivial to keep in sync by inspection.
