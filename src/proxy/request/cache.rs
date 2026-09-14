//! Proxy response caching: `request_cache_filter`, `should_serve_stale`,
//! `cache_key_callback` -- the [`pingora_proxy::ProxyHttp`] trait-method
//! bodies for Pingora's cache hooks.
//!
//! Split out of the former monolithic `request_phase.rs` (issue #144 prep,
//! PR 1 of 2) -- pure code relocation, no behavioral change.

use pingora_cache::CacheKey;
use pingora_core::Result;
use pingora_proxy::Session;

use crate::proxy::cache as proxy_cache;
#[cfg(feature = "cache")]
use crate::proxy::cache_disk;
// Only used inside request_cache_filter's `#[cfg(feature = "cache")]` body
// below -- `redis` alone (e.g. for the Redis-backed rate limiter, which
// doesn't touch this import at all) must not pull in an unused import under
// `-D warnings` (issue #312).
#[cfg(all(feature = "redis", feature = "cache"))]
use crate::proxy::cache_redis;
use crate::proxy::ctx::RequestCtx;
use crate::proxy::request::transform::extract_host;
use crate::proxy::service::ConduitProxy;

#[cfg(feature = "cache")]
use pingora_cache::storage::Storage as CacheStorage;

/// Body of [`pingora_proxy::ProxyHttp::request_cache_filter`].
///
/// Enable the cache for proxy routes that carry a `cache` configuration.
///
/// Called by Pingora after `request_filter`; only reached for upstream-bound
/// requests (local handlers return `Ok(true)` in `request_filter`).
pub(crate) fn request_cache_filter(
    _proxy: &ConduitProxy,
    session: &mut Session,
    ctx: &mut Option<RequestCtx>,
) -> Result<()> {
    // The full cache implementation is only compiled when --features cache is active.
    #[cfg(feature = "cache")]
    {
        let Some(req_ctx) = ctx.as_ref() else {
            return Ok(());
        };
        let Some(ref cfg) = req_ctx.proxy.proxy_cache_cfg else {
            return Ok(());
        };

        // Select storage backend based on store string.
        let storage: &'static (dyn CacheStorage + Sync) = if cfg.store == "memory" {
            proxy_cache::cache_storage()
        } else if cfg.store.starts_with("redis://") || cfg.store.starts_with("rediss://") {
            #[cfg(feature = "redis")]
            {
                match cache_redis::get(&cfg.store) {
                    Some(s) => s,
                    None => {
                        tracing::warn!(
                            store = %cfg.store,
                            "Redis cache unavailable — caching disabled for this request"
                        );
                        return Ok(());
                    }
                }
            }
            #[cfg(not(feature = "redis"))]
            {
                tracing::warn!(
                    store = %cfg.store,
                    "Redis cache requires --features redis — caching disabled"
                );
                return Ok(());
            }
        } else if let Some(dir) = cfg.store.strip_prefix("disk:") {
            cache_disk::get_or_create(dir)
        } else {
            tracing::warn!(
                store = %cfg.store,
                "unsupported cache store — caching disabled for this route"
            );
            return Ok(());
        };

        // Check request-side policy (method, cookies, authorization, skip-paths).
        let method = session.req_header().method.as_str();
        let path = session.req_header().uri.path();
        let has_cookie = session.req_header().headers.contains_key("cookie");
        let has_authorization = session.req_header().headers.contains_key("authorization");

        if !proxy_cache::should_cache_request(cfg, method, has_cookie, has_authorization, path) {
            return Ok(());
        }

        // Pass the cache-key lock to prevent thundering herd on cache miss:
        // only one request fetches from upstream; concurrent requests wait for
        // the cached response instead of all hitting the upstream at once.
        session
            .cache
            .enable(storage, None, None, Some(proxy_cache::cache_lock()), None);
    }
    // Without --features cache the entire block above is absent and we fall through.
    #[cfg(not(feature = "cache"))]
    let _ = (session, ctx);
    Ok(())
}

/// Body of [`pingora_proxy::ProxyHttp::should_serve_stale`].
///
/// Stale-while-revalidate / stale-if-error policy.
///
/// - When `error` is `None` Pingora is asking whether to serve a stale
///   response while background revalidation runs (SWR).  We return `true`
///   when the route's `staleWhileRevalidateSecs` is non-zero — Pingora has
///   already checked the stale window via `CacheMeta.serve_stale_while_revalidate()`.
///
/// - When `error` is `Some(_)` Pingora is asking whether to serve stale on
///   upstream error (stale-if-error).  We return `true` when
///   `staleIfErrorSecs` is non-zero and the error comes from upstream.
pub(crate) fn should_serve_stale(
    ctx: &Option<RequestCtx>,
    error: Option<&pingora_core::Error>,
) -> bool {
    let Some(req_ctx) = ctx.as_ref() else {
        return false;
    };
    let Some(ref cfg) = req_ctx.proxy.proxy_cache_cfg else {
        return false;
    };
    match error {
        // SWR: serve stale while revalidating if window is configured.
        None => cfg.stale_while_revalidate_secs.unwrap_or(0) > 0,
        // Stale-if-error: serve stale on upstream failure.
        Some(e) => {
            cfg.stale_if_error_secs.unwrap_or(0) > 0
                && e.esource() == &pingora_core::ErrorSource::Upstream
        }
    }
}

/// Body of [`pingora_proxy::ProxyHttp::cache_key_callback`].
///
/// Build a deterministic cache key: namespace = Host header, primary = scheme:path[?query].
pub(crate) fn cache_key_callback(
    proxy: &ConduitProxy,
    session: &Session,
    ctx: &mut Option<RequestCtx>,
) -> Result<CacheKey> {
    // Use extract_host() so the port suffix is stripped (e.g. "example.com:8080" → "example.com").
    // This keeps the cache key consistent with how the router matches virtual hosts.
    let host_str = extract_host(session);
    let host = host_str.as_str();

    // Derive scheme from whether the matched site has TLS configured.
    let scheme = {
        let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
        let config = proxy.state.config.load();
        if config
            .sites
            .get(site_idx)
            .and_then(|s| s.tls.as_ref())
            .is_some()
        {
            "https"
        } else {
            "http"
        }
    };

    let uri = &session.req_header().uri;
    let path = uri.path();
    let query = uri.query().filter(|q| !q.is_empty());

    // Vary-based cache key differentiation: include the specified request
    // header values so that different representations are stored separately.
    let vary_headers = {
        let site_idx = ctx.as_ref().map(|c| c.site_idx).unwrap_or(0);
        let config = proxy.state.config.load();
        config.sites.get(site_idx).and_then(|_s| {
            ctx.as_ref()
                .and_then(|c| c.proxy.proxy_cache_cfg.as_ref())
                .and_then(|cc| cc.vary_headers.clone())
        })
    };

    Ok(proxy_cache::build_cache_key(
        host,
        scheme,
        path,
        query,
        vary_headers.as_deref(),
        Some(&session.req_header().headers),
    ))
}
