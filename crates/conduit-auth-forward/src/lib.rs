//! Forward-auth crate for conduit's feature-driven Cargo workspace migration
//! (issue [#114](https://github.com/lopatnov/conduit/issues/114), extracted
//! in [#134](https://github.com/lopatnov/conduit/issues/134)).
//!
//! ## Scope
//!
//! Owns [`ForwardAuthConfig`] (the `sites[].forwardAuth` config struct) and
//! the real `guard::ForwardAuthGuard` — a request guard that delegates
//! authentication/authorization to an external HTTP service, forwarding a
//! subset of the request and honoring the auth service's 2xx/4xx/5xx
//! response.
//!
//! `ForwardAuthConfig` is compiled into **every** conduit build — like
//! `AcmeConfig`/`OtlpConfig`/`TcpConfig`/`UploadConfig`/`FaultInjectionConfig`/
//! `JwtAuthConfig` — because `SiteConfig.forward_auth` is not itself
//! feature-gated (a config file that sets `forwardAuth` without
//! `--features forward-auth` must still parse cleanly and get an explicit
//! `feature_warnings()` warning, not a silent-drop or a hard parse error).
//! Only the real `guard::ForwardAuthGuard` — and its process-wide
//! `reqwest::Client` singleton — is gated behind this crate's own
//! `forward-auth` Cargo feature; the root crate's `forward-auth` feature
//! forwards into it via `lopatnov-conduit-auth-forward/forward-auth`.
//!
//! ## Guard-shaped extraction (#132/#133's pattern, reused here)
//!
//! Like `conduit-faults` (#132) and `conduit-auth-jwt` (#133),
//! `guard::ForwardAuthGuard` implements `conduit-core`'s
//! [`RequestFilter`](conduit_core::filter::chain::RequestFilter) chain trait
//! directly, so per `CONTRIBUTING.md`'s crate extraction recipe this crate
//! *does* depend on `lopatnov-conduit-core`, gated behind the same
//! `forward-auth` feature as the rest of the guard's dependencies. Chain
//! assembly and guard ordering stay in the root crate's
//! `src/filter/chain.rs` (`CLAUDE.md` decision #20) — this crate exports
//! only the filter implementation and its constructor input
//! ([`ForwardAuthConfig`]), never a chain position.
//!
//! This is a clean, full extraction — unlike the companion #134 crate
//! `conduit-auth-consumers`, ForwardAuthGuard doesn't reach into any
//! not-yet-extracted root-crate state (no rate limiter, no per-request
//! `RequestCtx` field), so the whole guard moves here without a partial
//! split.

pub mod config;
#[cfg(feature = "forward-auth")]
pub mod guard;

pub use config::ForwardAuthConfig;
