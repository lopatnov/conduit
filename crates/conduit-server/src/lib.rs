//! Server bootstrap, Admin API supervisor and config validation/providers for conduit's feature-driven Cargo
//! workspace (issue [#114](https://github.com/lopatnov/conduit/issues/114), extracted in
//! [#147](https://github.com/lopatnov/conduit/issues/147)).
//!
//! ## What is here
//!
//! - [`server`] — `run_server()` (Pingora bootstrap, TLS/plain listener wiring, ACME procurement,
//!   Redis rate-limiter connect, TCP/redirect services), plus `tls`/`redirect`/`acme`/`otel`/`shutdown`.
//! - [`admin`] — `AdminApiService`, the process's background supervisor (rate-limit cleanup, health
//!   probes/warmup, Redis cache connect, hot-reload watcher, event-loop lag gauge) and `POST /reload`.
//!   The other eleven Admin API endpoints stay in `crates/conduit-admin` (issue #146), which this crate
//!   depends on for `serve`/`AdminError`/`AdminResult`/`validate_cert_key_pem`.
//! - [`config`] — `defaults`, `provider` (`FileProvider`, `load_and_validate`), `kubernetes`
//!   (`KubernetesProvider`, behind the `kubernetes` feature), `rate_limit_scan` (shared Redis-store walk) and
//!   the real `validate` logic (`validate()`/`feature_warnings()` and their auth/cross_site/proxy_loop/site/tls/
//!   warnings modules).
//!
//! ## What deliberately stays in the root crate
//!
//! `src/config/validate/mod.rs` in the root is a facade re-exporting `validate`/`feature_warnings` from here,
//! but it *also* keeps the seventeen `conduit_x::warnings::COMPILED == cfg!(feature = "x")` compile-time asserts
//! word-for-word: those check the root crate's OWN Cargo features against every feature-owning crate
//! (`otlp`/`wasm`/`rhai`/`jwt`/`forward-auth`/`compression`/`static`/... — several of which this crate has no
//! other reason to depend on), and `cfg!()` always resolves against the crate physically compiling the
//! assert. Moving them here would force this crate to mirror all sixteen names just to keep the asserts
//! meaningful, for zero runtime benefit — the warning-dispatch functions below never read their own crate's
//! `cfg!()`, they only call into each leaf crate's `warnings::feature_warning()`/`COMPILED`, which Cargo's
//! per-package feature unification already makes correct regardless of which crate calls them. The root's
//! `tests.rs`/`golden_tests.rs`/`testdata/` stay in the root for the same reason: they pin `validate()`'s and
//! `feature_warnings()`'s output against the ROOT's actual compiled feature set.
//!
//! ## Features
//!
//! Ten root features gate code in this crate; each is declared here with the same forwards as in the root, and
//! [`features`] exposes them as constants so the root can assert at compile time that the two agree (a root
//! feature that is on while this crate's is off would compile and quietly drop a code path — the #30/#145
//! hazard).

pub mod admin;
pub mod config;
pub mod server;

/// This crate's own view of the ten features it hosts, for the root's parity asserts.
pub mod features {
    pub const PROXY: bool = cfg!(feature = "proxy");
    pub const REDIS: bool = cfg!(feature = "redis");
    pub const CONSUMERS: bool = cfg!(feature = "consumers");
    pub const CACHE: bool = cfg!(feature = "cache");
    pub const ACME: bool = cfg!(feature = "acme");
    pub const TCP: bool = cfg!(feature = "tcp");
    pub const UPLOAD: bool = cfg!(feature = "upload");
    pub const HOTRELOAD: bool = cfg!(feature = "hotreload");
    pub const TOKIO_METRICS: bool = cfg!(feature = "tokio-metrics");
    pub const KUBERNETES: bool = cfg!(feature = "kubernetes");
}

// Aliases for the paths the moved files used in the root crate — same device as `conduit-runtime`/`conduit-admin`.
mod filter {
    #[cfg(feature = "redis")]
    pub(crate) use conduit_ratelimit::redis as rate_limit_redis;
    pub(crate) use conduit_runtime::filter::rate_limit;
}

mod proxy {
    #[cfg(all(feature = "cache", feature = "redis"))]
    pub(crate) use conduit_runtime::proxy::cache_redis;
    pub(crate) use conduit_runtime::proxy::service;
    #[cfg(feature = "proxy")]
    pub(crate) use conduit_runtime::proxy::{health, upstream};
    #[cfg(feature = "tcp")]
    pub(crate) use conduit_tcp::proxy as tcp;
}

mod handler {
    #[cfg(feature = "hotreload")]
    pub(crate) use conduit_runtime::handler::hot_reload;
}

#[cfg(feature = "upload")]
mod upload {
    pub(crate) use conduit_runtime::upload::UploadService;
}
