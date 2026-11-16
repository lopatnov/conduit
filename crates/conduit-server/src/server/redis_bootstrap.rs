use std::sync::Arc;

use crate::config::schema::AppConfig;
use conduit_config_core::redact::redact_url;
use conduit_ratelimit::redis::RedisRateLimiter;

use crate::config::rate_limit_scan::find_redis_rate_limit_store;

/// Connect to Redis for rate limiting if the operator set an address, or any site, route, or consumer has a
/// `redis://` store configured.
///
/// A temporary single-threaded Tokio runtime is used for the async handshake so
/// this can run from the synchronous `run_server`.  Connection failures are logged
/// as warnings and the server falls back to the in-memory limiter.
pub(super) fn connect_redis_rate_limiter_if_configured(
    config: &AppConfig,
) -> anyhow::Result<Option<Arc<RedisRateLimiter>>> {
    // The operator's address (issue #760) is recorded first: a bare `store: redis` of a cache or a limit resolves
    // to it for the life of the process (the connections are made now; a later change takes a restart).
    conduit_config_core::scheme::set_operator_redis_url(
        crate::config::rate_limit_scan::operator_redis_address(config),
    );
    crate::config::rate_limit_scan::record_startup_redis_store(config);
    let url_opt = find_redis_rate_limit_store(config);
    let Some(ref url) = url_opt else {
        return Ok(None);
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("cannot build tokio runtime for Redis: {e}"))?;
    match rt.block_on(RedisRateLimiter::connect(url)) {
        Ok(rrl) => {
            tracing::info!(url = %redact_url(url), "Redis rate limiter connected");
            Ok(Some(Arc::new(rrl)))
        }
        Err(e) => {
            tracing::warn!(
                url = %redact_url(url),
                "Redis rate limiter unavailable: {e} — using memory fallback"
            );
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    // ── find_redis_rate_limit_store (issue #322) ────────────────────────────

    use indexmap::IndexMap;

    use super::*;
    use crate::config::rate_limit_scan::find_redis_rate_limit_store_in;
    use crate::config::schema::{
        ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, RateLimitConfig, SiteConfig,
    };

    fn memory_rate_limit() -> RateLimitConfig {
        RateLimitConfig {
            window_secs: 60,
            limit: 10,
            burst: None,
            algorithm: None,
            key_by: None,
            skip_paths: None,
            store: None,
            dry_run: None,
        }
    }

    fn redis_rate_limit(url: &str) -> RateLimitConfig {
        RateLimitConfig {
            store: Some(url.to_owned()),
            ..memory_rate_limit()
        }
    }

    #[test]
    fn finds_nothing_when_no_site_configures_a_store() {
        let config = AppConfig {
            sites: vec![SiteConfig {
                rate_limit: Some(memory_rate_limit()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(find_redis_rate_limit_store_in(&config, None), None);
    }

    #[test]
    fn finds_site_level_redis_store() {
        let config = AppConfig {
            sites: vec![SiteConfig {
                rate_limit: Some(redis_rate_limit("redis://127.0.0.1:6379")),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            find_redis_rate_limit_store_in(&config, None).as_deref(),
            Some("redis://127.0.0.1:6379")
        );
    }

    /// Issue #760: a bare `store: redis` is the operator's address, wherever the limit sits; an
    /// operator's address wins over a legacy URL; no redis store, no address needed.
    #[test]
    fn a_bare_redis_store_connects_to_the_operators_address() {
        let global = |url: &str| {
            Some(crate::config::schema::GlobalConfig {
                redis: Some(crate::config::schema::RedisConfig {
                    url: Some(url.to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            })
        };
        let site = |store: &str| SiteConfig {
            rate_limit: Some(redis_rate_limit(store)),
            ..Default::default()
        };
        let bare = AppConfig {
            global: global("rediss://redis.internal:6380"),
            sites: vec![site("redis")],
            ..Default::default()
        };
        assert_eq!(
            find_redis_rate_limit_store_in(&bare, None).as_deref(),
            Some("rediss://redis.internal:6380")
        );
        // a legacy URL equal to the address, next to a bare one: still the operator's address
        let mixed = AppConfig {
            global: global("redis://a:6379"),
            sites: vec![site("redis://a:6379"), site("redis")],
            ..Default::default()
        };
        assert_eq!(
            find_redis_rate_limit_store_in(&mixed, None).as_deref(),
            Some("redis://a:6379")
        );
        // an address and no redis store yet: still connected, so a store added later shares the limit (#763 review)
        let unused = AppConfig {
            global: global("redis://a:6379"),
            sites: vec![SiteConfig {
                rate_limit: Some(memory_rate_limit()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            find_redis_rate_limit_store_in(&unused, None).as_deref(),
            Some("redis://a:6379")
        );
    }

    #[test]
    fn finds_route_level_redis_store_with_no_site_level_store() {
        // The gap issue #322 actually reported: a site with Redis
        // configured only on a route, not at the site level, must still
        // trigger a Redis connection at startup — before this fix,
        // `connect_redis_rate_limiter_if_configured` only ever looked at
        // `SiteConfig.rate_limit`, so this case silently fell back to the
        // in-memory limiter forever.
        let mut routes = IndexMap::new();
        routes.insert(
            "/api".to_owned(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                rate_limit: Some(redis_rate_limit("redis://route-only:6379")),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            find_redis_rate_limit_store_in(&config, None).as_deref(),
            Some("redis://route-only:6379")
        );
    }

    #[test]
    #[cfg(feature = "consumers")]
    fn finds_consumer_level_redis_store_with_no_site_or_route_level_store() {
        use conduit_auth_consumers::{Consumer, ConsumersConfig};

        let config = AppConfig {
            sites: vec![SiteConfig {
                consumers: Some(ConsumersConfig {
                    consumers: vec![Consumer {
                        username: "alice".to_owned(),
                        api_key: None,
                        basic_auth: None,
                        jwt: None,
                        rate_limit: Some(redis_rate_limit("rediss://consumer-only:6380")),
                        headers: None,
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            find_redis_rate_limit_store_in(&config, None).as_deref(),
            Some("rediss://consumer-only:6380")
        );
    }

    #[test]
    fn site_level_store_wins_when_multiple_levels_configure_one() {
        let mut routes = IndexMap::new();
        routes.insert(
            "/api".to_owned(),
            ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
                rate_limit: Some(redis_rate_limit("redis://route:6379")),
                ..Default::default()
            })),
        );
        let config = AppConfig {
            sites: vec![SiteConfig {
                rate_limit: Some(redis_rate_limit("redis://site:6379")),
                proxy: Some(ProxyConfig::Routes(routes)),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            find_redis_rate_limit_store_in(&config, None).as_deref(),
            Some("redis://site:6379")
        );
    }
}
