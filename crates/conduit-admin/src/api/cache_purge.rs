//! `DELETE /cache/purge`.

use axum::extract::Query;
use axum::Json;
use serde::Deserialize;
#[cfg(feature = "cache")]
use serde_json::json;
use serde_json::Value;

use super::error::{AdminError, AdminResult};

// ── Cache purge ───────────────────────────────────────────────────────────────

/// Query parameters for `DELETE /cache/purge`.
#[derive(Deserialize)]
pub(super) struct CachePurgeParams {
    /// Full URL to purge, e.g. `https://example.com/api/data?page=1`
    #[cfg_attr(not(feature = "cache"), allow(dead_code))]
    pub(super) url: String,
}

/// The cache key that a purge of `raw` (a full `http://`/`https://` URL) has to target.
///
/// The key's host is the URL's host **without its port** — `build_cache_key` drops a port, exactly
/// as it does for the `Host` header on the request path — so `http://example.com:8080/x` finds the
/// entry stored for a request to `example.com:8080` (issue #444: the two used to disagree, the purge
/// answered `purged:false` and the stale entry survived).
#[cfg(feature = "cache")]
pub(super) fn purge_cache_key(raw: &str) -> Result<pingora_cache::CacheKey, AdminError> {
    // Use the url crate for robust parsing (handles IPv6, query-only URLs, etc.)
    let parsed =
        url::Url::parse(raw).map_err(|e| AdminError::BadRequest(format!("invalid url: {e}")))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(AdminError::BadRequest(
            "url must start with http:// or https://".to_owned(),
        ));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| AdminError::BadRequest("url has no host".to_owned()))?;

    Ok(conduit_runtime::proxy::cache::build_cache_key(
        host,
        scheme,
        parsed.path(),
        parsed.query(),
        None,
        None,
    ))
}

/// `DELETE /cache/purge?url=<url>` — invalidate a specific cache entry.
///
/// Parses the URL into its components, builds the same `CacheKey` that the
/// proxy would use, and calls `MemCache::purge()` on the shared storage.
///
/// Returns `{"status":"ok","purged":true}` when an entry was found and removed,
/// `{"status":"ok","purged":false}` when no matching entry existed, or an error
/// JSON on bad input.
///
/// The `cache` variant. Without the feature there is no response cache to
/// purge -- and the `url` crate this handler parses with is not compiled in
/// (issue #144, PR 4b) -- so the route stays registered but answers 501, see
/// the no-`cache` variant below.
#[cfg(feature = "cache")]
pub(super) async fn cache_purge_handler(
    Query(params): Query<CachePurgeParams>,
) -> AdminResult<Json<Value>> {
    use pingora_cache::storage::{PurgeOutcome, PurgeTarget, PurgeType, Storage};
    use pingora_cache::trace::Span;

    let raw = params.url.trim();
    let cache_key = purge_cache_key(raw)?;
    let compact = cache_key.to_compact();
    let storage = conduit_runtime::proxy::cache::cache_storage();

    let span = Span::inactive().handle();
    let outcome = storage
        .purge(
            PurgeTarget::Active(&compact),
            PurgeType::Invalidation,
            &span,
        )
        .await
        .map_err(|e| AdminError::ServerError(format!("cache purge failed: {e}")))?;
    let purged = matches!(outcome, PurgeOutcome::Purged(_));

    Ok(Json(
        json!({ "status": "ok", "purged": purged, "url": raw }),
    ))
}

/// No-`cache` variant of [`cache_purge_handler`]: a build without the `cache`
/// feature has no response cache, so answering `{"purged":false}` (what the
/// real handler would say for an entry that never existed) would be a silent
/// lie about a store nothing writes to. 501 says what is actually true.
#[cfg(not(feature = "cache"))]
pub(super) async fn cache_purge_handler(
    Query(_params): Query<CachePurgeParams>,
) -> AdminResult<Json<Value>> {
    Err(AdminError::NotImplemented(
        "cache purge is unavailable: this Conduit was built without the `cache` feature, \
         so there is no response cache to purge"
            .to_owned(),
    ))
}
