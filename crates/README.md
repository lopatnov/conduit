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
  `is_path_skipped`, `AcceptEncoding`, `content_type`, `LogWriter`). Compiled
  into every build regardless of feature selection — the root crate's `src/`
  re-exports these through thin facades; concrete guards/filters and
  config-aware chain assembly stay in the root crate.

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
