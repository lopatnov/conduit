//! Fault-injection (chaos testing) crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#132](https://github.com/lopatnov/conduit/issues/132)).
//!
//! ## Scope
//!
//! Owns [`FaultInjectionConfig`] (the `sites[].faultInjection` config
//! struct, together with [`FaultAbort`]/[`FaultDelay`]) and the real
//! `guard::FaultInjectionGuard` — a request guard that aborts or delays a
//! configurable percentage of requests, used for chaos-engineering and
//! testing retry/circuit-breaker behaviour without a real failing upstream.
//! `FaultInjectionConfig` is compiled into **every** conduit build — like
//! `AcmeConfig`/`OtlpConfig`/`TcpConfig`/`UploadConfig` — because
//! `SiteConfig.fault_injection` is not itself feature-gated (a config file
//! that sets `faultInjection` without `--features fault-injection` must
//! still parse cleanly and get an explicit `feature_warnings()` warning, not
//! a silent-drop or a hard parse error). Only the real
//! `guard::FaultInjectionGuard` is gated behind this crate's own
//! `fault-injection` Cargo feature; the root crate's `fault-injection`
//! feature forwards into it via `lopatnov-conduit-faults/fault-injection`.
//!
//! ## The "smallest guard-shaped extraction" (#132)
//!
//! Unlike the handler/service-shaped `conduit-otlp`/`conduit-acme`
//! extractions, `guard::FaultInjectionGuard` implements `conduit-core`'s
//! [`RequestFilter`](conduit_core::filter::chain::RequestFilter) chain trait
//! directly — the same trait every other in-chain guard implements — so per
//! `CONTRIBUTING.md`'s crate extraction recipe ("conduit-core dependency is
//! opt-in, not automatic"), this crate *does* depend on
//! `lopatnov-conduit-core`, gated behind the same `fault-injection` feature
//! as the rest of the guard's dependencies (`conduit-acme`'s `challenge`
//! module was the first crate to take this dependency; this is the second).
//! Chain assembly and guard ordering stay in the root crate's
//! `src/filter/chain.rs` (`CLAUDE.md` decision #20) — this crate exports
//! only the filter implementation and its constructor input
//! ([`FaultInjectionConfig`]), never a chain position.

pub mod config;
#[cfg(feature = "fault-injection")]
pub mod guard;

pub use config::{FaultAbort, FaultDelay, FaultInjectionConfig};
