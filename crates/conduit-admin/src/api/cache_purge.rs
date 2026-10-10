//! `DELETE /cache/purge`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
#[cfg(feature = "cache")]
use serde_json::json;
use serde_json::Value;

use super::error::{AdminError, AdminResult};
#[cfg(feature = "cache")]
use conduit_config::schema::SiteConfig;
use conduit_runtime::proxy::service::AppState;

// ── Cache purge ───────────────────────────────────────────────────────────────

/// Query parameters for `DELETE /cache/purge`.
#[derive(Deserialize)]
pub(super) struct CachePurgeParams {
    /// Full URL to purge, e.g. `https://example.com/api/data?page=1`
    #[cfg_attr(not(feature = "cache"), allow(dead_code))]
    pub(super) url: String,
}

/// The listener ports the proxy serves, which the cache key names (issue #482): each site's `port`,
/// defaulting like `classify_ports` does (443 with `tls`, else 80), or 8080 when no site is configured.
#[cfg(feature = "cache")]
pub(super) fn listener_ports(sites: &[SiteConfig]) -> Vec<u16> {
    let mut ports: Vec<u16> = if sites.is_empty() {
        vec![8080]
    } else {
        sites
            .iter()
            .filter(|s| s.tcp.is_none())
            .map(|s| s.port.unwrap_or(if s.tls.is_some() { 443 } else { 80 }))
            .collect()
    };
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// The cache keys that a purge of `raw` (a full `http://`/`https://` URL) has to target.
///
/// The key's host is the URL's host **without its port** (`build_cache_key` drops it, exactly as it
/// does for the `Host` header on the request path), so `http://example.com:8080/x` finds the entry
/// stored for a request to `example.com:8080` (issue #444). The key *also* names the listener port
/// the site serves on (issue #482), which a URL does not have to carry: an explicit URL port picks
/// that one site's entry, and a URL without a port purges the entry of every listener port in
/// `ports` (a proxy behind a port mapping serves `example.com` on 8080, not 80). A port without an
/// entry is a harmless miss.
#[cfg(feature = "cache")]
pub(super) fn purge_cache_keys(
    raw: &str,
    ports: &[u16],
) -> Result<Vec<pingora_cache::CacheKey>, AdminError> {
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

    // `Url::port()` is `None` for the scheme's default port, which is written or omitted alike.
    let explicit = parsed.port();
    let targets: Vec<u16> = match explicit {
        Some(p) => vec![p],
        None => ports.to_vec(),
    };

    Ok(targets
        .into_iter()
        .map(|port| {
            conduit_runtime::proxy::cache::build_cache_key(
                host,
                port,
                scheme,
                parsed.path(),
                parsed.query(),
                None,
                None,
            )
        })
        .collect())
}

/// `DELETE /cache/purge?url=<url>` — invalidate a specific cache entry.
///
/// Parses the URL into its components, builds the same `CacheKey`s that the
/// proxy would use (one per listener port when the URL names none, see
/// [`purge_cache_keys`]), and calls `MemCache::purge()` on the shared storage.
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
    State(state): State<Arc<AppState>>,
    Query(params): Query<CachePurgeParams>,
) -> AdminResult<Json<Value>> {
    use pingora_cache::storage::{PurgeOutcome, PurgeTarget, PurgeType, Storage};
    use pingora_cache::trace::Span;

    let raw = params.url.trim();
    let ports = listener_ports(&state.config.load().sites);
    let cache_keys = purge_cache_keys(raw, &ports)?;
    let storage = conduit_runtime::proxy::cache::cache_storage();

    let span = Span::inactive().handle();
    let mut purged = false;
    for cache_key in &cache_keys {
        let compact = cache_key.to_compact();
        let outcome = storage
            .purge(
                PurgeTarget::Active(&compact),
                PurgeType::Invalidation,
                &span,
            )
            .await
            .map_err(|e| AdminError::ServerError(format!("cache purge failed: {e}")))?;
        purged |= matches!(outcome, PurgeOutcome::Purged(_));
    }

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
    State(_state): State<Arc<AppState>>,
    Query(_params): Query<CachePurgeParams>,
) -> AdminResult<Json<Value>> {
    Err(AdminError::NotImplemented(
        "cache purge is unavailable: this Conduit was built without the `cache` feature, \
         so there is no response cache to purge"
            .to_owned(),
    ))
}
