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
  concern.

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
