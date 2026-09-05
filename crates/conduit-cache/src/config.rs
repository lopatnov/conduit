//! `CacheConfig` — the `proxy.*.cache` config struct.
//!
//! Compiled into every build (see `lib.rs` doc comment): a config file that
//! sets `cache` without `--features cache` must still parse cleanly and get
//! an explicit `feature_warnings()` warning from the root crate, not a
//! silent-drop or a hard parse error.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheConfig {
    /// "memory" | "redis://..." | "disk:./cache"
    pub store: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    /// Serve stale content while revalidating in the background (RFC 5861).
    ///
    /// When set, a cache entry that has expired within the stale window is
    /// returned immediately to the client while Pingora fetches a fresh copy
    /// asynchronously.  The next request after revalidation completes gets
    /// the fresh copy.  Zero perceived latency for the overwhelming majority
    /// of requests.
    ///
    /// ```yaml
    /// cache:
    ///   store: memory
    ///   ttlSecs: 60
    ///   staleWhileRevalidateSecs: 300  # serve stale for up to 5 min after TTL expires
    /// ```
    #[serde(
        rename = "staleWhileRevalidateSecs",
        skip_serializing_if = "Option::is_none"
    )]
    pub stale_while_revalidate_secs: Option<u32>,
    /// Serve stale content when the upstream returns an error (RFC 5861
    /// `stale-if-error`).
    ///
    /// When upstream is unreachable or returns 5xx, Conduit serves the most
    /// recently cached copy for up to `staleIfErrorSecs` seconds instead of
    /// forwarding the error to the client.
    #[serde(rename = "staleIfErrorSecs", skip_serializing_if = "Option::is_none")]
    pub stale_if_error_secs: Option<u32>,
    /// Proactively refresh a cache entry in the background before it expires.
    ///
    /// When the remaining TTL drops below `earlyRefreshSecs`, Conduit fires a
    /// fire-and-forget GET request to the upstream.  The client is served the
    /// still-valid cached response with zero latency — the refresh happens
    /// concurrently.  The cache is updated the moment the background response
    /// arrives, so the next real request always gets a fresh copy.
    ///
    /// Unlike `staleWhileRevalidateSecs`, which activates only *after* the TTL
    /// expires, `earlyRefreshSecs` ensures the cache never goes stale from the
    /// client's perspective.
    ///
    /// ```yaml
    /// cache:
    ///   store: memory
    ///   ttlSecs: 60
    ///   earlyRefreshSecs: 10  # refresh 10 s before expiry
    /// ```
    #[serde(rename = "earlyRefreshSecs", skip_serializing_if = "Option::is_none")]
    pub early_refresh_secs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vary_headers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_if_cookie: Option<bool>,
    /// Default: ["GET", "HEAD"]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub methods: Option<Vec<String>>,
}
