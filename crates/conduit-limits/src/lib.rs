//! Request-size/inflight/per-IP/upload-rate limits crate for conduit's
//! feature-driven Cargo workspace migration (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted as the
//! second half of [#137](https://github.com/lopatnov/conduit/issues/137) —
//! the `conduit-ratelimit` half of that issue was already extracted
//! separately and covers a different, similarly-named config type).
//!
//! ## Scope
//!
//! Owns [`LimitsConfig`] (the `sites[].limits` config struct), the pure
//! limit-checking logic (`limits` module — declared-Content-Length /
//! header-size checks, and the leaky-bucket minimum-upload-rate algorithm
//! from issue [#51](https://github.com/lopatnov/conduit/issues/51)), the
//! real `guard::LimitsGuard` chain guard (Host-header validation,
//! `maxRequestHeaders`, `maxInflightRequests`, body/header size limits, and
//! `maxConnectionsPerIp` via the RAII `guard::IpConnSlotGuard`), and
//! [`LimitsReqState`] — the per-request state `RequestCtx` threads through
//! the request-body pipeline.
//!
//! ## Always-on, no Cargo feature (like ipFilter/cors/securityHeaders)
//!
//! Unlike `conduit-faults`/`conduit-auth-jwt`/`conduit-acme`/etc., `limits`
//! is **not** gated behind any Cargo feature at all — see `CLAUDE.md`
//! architectural decision #31 (2026-08-23): request-size/inflight/per-IP
//! limits stay always-on/default-on for the same reason `ipFilter`/`cors`/
//! `securityHeaders` do — gating them buys almost no binary-size benefit
//! (light logic, no heavy third-party dependency) while adding a real
//! "forgot the flag, silently stopped limiting" risk for a security-relevant
//! guard. Concretely: this crate has **no `[features]` table**, and every
//! dependency below — including `lopatnov-conduit-core` for
//! `guard::LimitsGuard` — is a mandatory, non-optional dependency. This
//! mirrors `conduit-ipfilter`'s (#136) unconditional dependency style.
//!
//! `guard::LimitsGuard` implements `conduit-core`'s
//! [`RequestFilter`](conduit_core::filter::chain::RequestFilter) chain trait
//! directly, so per `CONTRIBUTING.md`'s crate extraction recipe ("conduit-core
//! dependency is opt-in, not automatic") this crate depends on
//! `lopatnov-conduit-core` — unconditionally here, since there's no feature
//! to gate it behind. Chain assembly and guard ordering stay in the root
//! crate's `src/filter/chain.rs` (`CLAUDE.md` decision #20) — this crate
//! exports only the filter implementation and its constructor inputs.
//!
//! ## [`LimitsReqState`] is unconditional, unlike `CacheReqState`/`JwtReqState`
//!
//! `RequestCtx` holds this crate's [`LimitsReqState`] as a plain,
//! always-present field (`pub limits: conduit_limits::LimitsReqState`) — not
//! wrapped in `Option<>` and not behind `#[cfg(feature = "...")]`, unlike
//! `conduit_cache::CacheReqState`/`conduit_auth_jwt::guard::JwtReqState`
//! (both gated because `cache`/`jwt` are optional Cargo features). `limits`
//! isn't optional, so there is no "feature not compiled in" state for this
//! field to represent — see `ctx.rs`'s doc comment for detail.
//!
//! ## Also closes issue #51 (no new functionality needed)
//!
//! Issue [#51](https://github.com/lopatnov/conduit/issues/51)
//! (`limits.minUploadRateBytesPerSec` slow-loris upload defense) was found
//! already fully implemented before this extraction — `min_upload_rate_bytes_per_sec`
//! in [`LimitsConfig`], the leaky-bucket `limits::upload_rate_step` algorithm,
//! its 7 unit tests, and its wiring into `request_body_filter` all already
//! existed and are simply relocated here unchanged.

pub mod config;
pub mod ctx;
pub mod guard;
pub mod limits;

pub use config::LimitsConfig;
pub use ctx::LimitsReqState;
