//! Per-request cache state (`CLAUDE.md` decision #30).
//!
//! Mirrors `conduit_auth_jwt::guard::JwtReqState` — a small state struct owned
//! by this crate that the root crate's `RequestCtx` holds behind a
//! `#[cfg(feature = "cache")]`-gated `Option<CacheReqState>` field, instead of
//! a type-erased extension slot. See `crates/conduit-auth-jwt/src/lib.rs`'s
//! doc comment for the rationale this pattern follows.

/// Per-request cache-related state, threaded through the response pipeline.
#[derive(Debug, Default)]
pub struct CacheReqState {
    /// Age in seconds to inject as the `Age` response header for cache hits
    /// (RFC 7234 §5.1). Computed in `upstream_response_filter` from the
    /// cached response's `Date` header: `age = now − date`.
    pub cache_age_secs: Option<u64>,
    /// Upstream URL to refresh in the background after this cache-hit
    /// response is served (early refresh, #31). Set by `response_filter`
    /// when the cache entry's remaining TTL is within `earlyRefreshSecs`;
    /// `logging()` spawns a fire-and-forget GET task for it.
    pub early_refresh_upstream_url: Option<String>,
}
