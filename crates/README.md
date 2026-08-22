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
