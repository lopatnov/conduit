//! Rate-limit config + token-bucket admission + Redis backend crate for
//! conduit's feature-driven Cargo workspace migration (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted across
//! two slices of [#137](https://github.com/lopatnov/conduit/issues/137)).
//!
//! ## Scope
//!
//! - **Slice 1** (always-on, no feature gate — mirrors `conduit-ipfilter`/
//!   `conduit-cors`/`conduit-security-headers`, `CLAUDE.md` decision #31's
//!   rationale extended here): [`RateLimitConfig`] (the shared `rateLimit`
//!   config struct — same shape at site/route/consumer level) and the pure,
//!   `Session`-independent token-bucket admission logic (`bucket` module:
//!   [`bucket::TokenBucket`], [`bucket::RateLimiter`], [`bucket::MAX_BUCKETS`],
//!   [`bucket::cleanup`], [`bucket::check_key`]).
//! - **Slice 2** (behind this crate's own `redis` feature, mirrors
//!   `conduit-cache`'s always-on-base + optional-`redis` shape): the
//!   Redis-backed limiter (`redis` module, [`redis::RedisRateLimiter`]) — a
//!   real algorithm difference from slice 1 (fixed-window counter, not a
//!   token bucket), moved from the pre-extraction `src/filter/
//!   rate_limit_redis.rs` in the root crate. Its key construction is now
//!   site-scoped (issue #317, the Redis-backend twin of the in-memory
//!   site-scoping fix from #303/#304).
//!
//! **Not yet moved here**:
//! - The `Session`-aware `extract_key`/`check` wrappers (`src/filter/
//!   rate_limit.rs` in the root crate) — these need `pingora_proxy::Session`
//!   and stay in the root crate, matching `conduit-ipfilter`'s precedent of
//!   keeping request-lifecycle-coupled code out of a Layer-0 crate.
//! - `conduit-limits` (a separate, unrelated config type despite the
//!   similar name — not in scope here at all).
//!
//! ## Why this exists now (not deferred to a full #137)
//!
//! `crates/conduit-auth-consumers/src/config.rs` carried a deliberate,
//! documented *temporary* duplicate of `RateLimitConfig` (issue #114/#134)
//! because a Layer-1 crate can't depend on a type living in the root crate
//! that depends on *it*. That duplication was flagged by SonarCloud
//! ("Duplicated Lines (%) on New Code") on the #114 tracking PR. Extracting
//! just the always-on slice (slice 1, this crate's initial scope) broke the
//! circular dependency without pulling the Redis/Session-aware work forward
//! — see the `architect` plan referenced from the PR that introduced this
//! crate for the full reasoning on why a minimal Layer-0 crate was chosen
//! over putting `RateLimitConfig` in `conduit-config-core` (which has a
//! documented zero-schema-knowledge invariant this would violate) or
//! `conduit-core`.

pub mod bucket;
pub mod config;
pub mod redis;

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
