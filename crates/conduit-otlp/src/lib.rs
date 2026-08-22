//! OpenTelemetry OTLP tracing crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#129](https://github.com/lopatnov/conduit/issues/129)).
//!
//! ## Scope
//!
//! Owns [`OtlpConfig`] (the `global.otlp` config struct) and the tracer
//! provider lifecycle ([`tracer::init_tracer`]/[`tracer::shutdown_tracer`]).
//! This crate is compiled into **every** conduit build — like
//! `conduit-core`/`conduit-config-core` — because `GlobalConfig.otlp` is not
//! itself feature-gated (a config file that sets `global.otlp` without
//! `--features otlp` must still parse cleanly and get an explicit
//! `feature_warnings()` warning, not a silent-drop or a hard parse error).
//! Only the *real* exporter wiring is gated behind this crate's own `otlp`
//! Cargo feature; without it, [`tracer::init_tracer`]/
//! [`tracer::shutdown_tracer`] are zero-overhead no-op stubs.
//!
//! ## What deliberately stays in the root crate
//!
//! Per-request span creation/finishing (`RequestCtx.otel_span`,
//! `src/proxy/request_phase.rs`, `src/proxy/logging_phase.rs`) is NOT part of
//! this crate — it needs `pingora_proxy::Session`/`RequestCtx`, which this
//! crate has no dependency on (see `CONTRIBUTING.md`'s crate extraction
//! recipe: "conduit-core dependency is opt-in, not automatic" — a crate with
//! no request-lifecycle behavior takes primitives instead). This mirrors
//! `CLAUDE.md`'s architectural decision #30 on `RequestCtx`: feature-owned
//! fields stay in the root crate, `#[cfg(feature = "x")]`-gated, same as
//! today.

pub mod config;
pub mod tracer;

pub use config::OtlpConfig;
