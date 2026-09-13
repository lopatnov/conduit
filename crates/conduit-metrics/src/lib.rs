//! Prometheus `/metrics` endpoint crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#140](https://github.com/lopatnov/conduit/issues/140)).
//!
//! ## Scope
//!
//! Owns [`MetricsConfig`] (the `sites[].metrics` config struct — optional
//! path override and bearer-token gate) and the real
//! `handler::MetricsHandler`/`handler::handle_metrics` — the Prometheus
//! text-exposition endpoint, moved from `src/handler/metrics.rs`.
//!
//! `ConduitMetrics` — the struct that *registers* every Prometheus metric
//! this proxy records (request counters, latency histograms, etc.) —
//! deliberately stays in the root crate (destined for the future
//! `conduit-runtime` crate, per issue #140's own scope note). This crate's
//! handler only *reads* the process-wide default registry via
//! `prometheus::gather()`, so it has no dependency on `ConduitMetrics` at
//! all.
//!
//! ## Always-on, no top-level Cargo feature (like `conduit-cors`/`conduit-
//! ipfilter`/`conduit-security-headers`/`conduit-redirects`)
//!
//! Per `CLAUDE.md` architectural decision #31 (2026-08-23), `metrics` stays
//! always-on/default-on — gating it buys almost no binary-size benefit
//! (`prometheus` is already an unconditional transitive dependency of
//! `pingora-core` regardless of this crate, confirmed via `cargo tree -i
//! prometheus`) while adding a real "forgot the flag, /metrics silently
//! stopped responding" risk. Unlike `conduit-cors`/`conduit-ipfilter`/
//! `conduit-security-headers` (no `[features]` table at all), this crate
//! *does* have one optional feature — `compression` — for the reason below.
//!
//! ## The `compression` sub-feature (issue #338)
//!
//! [`handler::MetricsHandler`] optionally compresses the Prometheus
//! text-exposition body via `conduit_compression::logic::compress_small_body`
//! — a small, independent Cargo feature on this crate (mirrors
//! `conduit-static`'s own independent `compression` sub-feature, #114/#139),
//! forwarded from the root crate's own (default-on) `compression` feature.
//! Unlike `conduit-static`, this crate never needs `async-compression`
//! directly — `compress_small_body` compresses an already-complete in-memory
//! body, no chunk-by-chunk streaming encoder pipeline.

pub mod config;
pub mod handler;

pub use config::MetricsConfig;
