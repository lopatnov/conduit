//! Rate-limit config + token-bucket admission crate for conduit's
//! feature-driven Cargo workspace migration (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted as
//! **slice 1** of [#137](https://github.com/lopatnov/conduit/issues/137)).
//!
//! ## Scope — slice 1 only
//!
//! Owns [`RateLimitConfig`] (the shared `rateLimit` config struct — same
//! shape at site/route/consumer level) and the pure, `Session`-independent
//! token-bucket admission logic (`bucket` module: [`bucket::TokenBucket`],
//! [`bucket::RateLimiter`], [`bucket::MAX_BUCKETS`], [`bucket::cleanup`],
//! [`bucket::check_key`]).
//!
//! **Not yet moved here** (later #137 slices):
//! - The Redis-backed limiter (`src/filter/rate_limit_redis.rs` in the root
//!   crate) — a real algorithm change (fixed-window counter, not a token
//!   bucket) plus a `redis` feature gate, deliberately not pulled forward
//!   just to close this slice's Sonar duplication finding.
//! - The `Session`-aware `extract_key`/`check` wrappers (`src/filter/
//!   rate_limit.rs` in the root crate) — these need `pingora_proxy::Session`
//!   and stay in the root crate, matching `conduit-ipfilter`'s precedent of
//!   keeping request-lifecycle-coupled code out of a Layer-0 crate.
//! - `conduit-limits` (a separate, unrelated config type despite the
//!   similar name — not in scope here at all).
//!
//! ## Always-on, no Cargo feature
//!
//! Like `conduit-ipfilter`/`conduit-cors`/`conduit-security-headers`
//! (`CLAUDE.md` decision #31's rationale extended here), rate limiting is
//! not gated behind a Cargo feature — this crate has **no `[features]`
//! table**, and both dependencies (`serde`, `dashmap`) plus `tracing` are
//! mandatory, non-optional. A future #137 slice adding the Redis backend
//! will add an optional `redis` feature at that point (mirrors
//! `conduit-cache`'s always-on-base + optional-`redis` shape) — this slice
//! deliberately doesn't reserve an empty `[features]` table for it ahead of
//! time.
//!
//! ## Why this exists now (not deferred to a full #137)
//!
//! `crates/conduit-auth-consumers/src/config.rs` carried a deliberate,
//! documented *temporary* duplicate of `RateLimitConfig` (issue #114/#134)
//! because a Layer-1 crate can't depend on a type living in the root crate
//! that depends on *it*. That duplication was flagged by SonarCloud
//! ("Duplicated Lines (%) on New Code") on the #114 tracking PR. Extracting
//! just the always-on slice (this crate) breaks the circular dependency
//! without pulling the Redis/Session-aware work forward — see the `architect`
//! plan referenced from the PR that introduced this crate for the full
//! reasoning on why a minimal Layer-0 crate was chosen over putting
//! `RateLimitConfig` in `conduit-config-core` (which has a documented
//! zero-schema-knowledge invariant this would violate) or `conduit-core`.

pub mod bucket;
pub mod config;

pub use bucket::{check_key, cleanup, RateLimiter, TokenBucket, MAX_BUCKETS};
pub use config::RateLimitConfig;

/// Admission check taking a [`RateLimitConfig`] directly, for the three
/// call sites that already have one (site/route/consumer rate limiting).
/// Thin wrapper over [`bucket::check_key`] — kept here rather than in
/// `bucket` so that module stays independent of [`config`].
pub fn check_key_for(limiter: &RateLimiter, key: &str, cfg: &RateLimitConfig) -> bool {
    check_key(
        limiter,
        key,
        cfg.limit,
        cfg.burst.unwrap_or(0),
        cfg.window_secs,
    )
}
