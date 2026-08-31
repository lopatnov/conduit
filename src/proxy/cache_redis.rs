#![cfg(feature = "redis")]
//! Extracted into `crates/conduit-cache` (issue #114/#135) — this is a
//! facade re-export so `crate::proxy::cache_redis::get` keeps resolving to
//! the same item at the same location for backward compatibility
//! (`src/proxy/request_phase.rs`). See `conduit_cache::redis` for the
//! implementation.
//!
//! This module also owns the config-walking glue that connects every
//! distinct `redis://`/`rediss://` `cache.store` URL up front, at server
//! startup and again on every hot reload (issue #330) — `conduit-cache`
//! itself has no knowledge of `AppConfig`/`SiteConfig` (see
//! `CONTRIBUTING.md`'s crate-extraction recipe), so the root crate has to
//! do the walking and hand bare URLs to `conduit_cache::redis`.

pub use conduit_cache::redis::get;

#[cfg(feature = "cache")]
use std::collections::BTreeSet;

#[cfg(feature = "cache")]
use crate::config::schema::{AppConfig, ProxyConfig, ProxyRouteTarget};

/// Every distinct `redis://`/`rediss://` `cache.store` URL configured
/// anywhere in `config` — both under the `proxy` shorthand and under
/// `routes[].proxy` (`ProxyRouteTarget::Full`'s `cache` field in either
/// place). Deduplicated and sorted so N routes sharing one URL connect it
/// once, and so iteration order is deterministic for tests.
#[cfg(feature = "cache")]
pub(crate) fn collect_redis_cache_urls(config: &AppConfig) -> Vec<String> {
    let mut urls = BTreeSet::new();

    let mut push_if_redis = |target: &ProxyRouteTarget| {
        if let ProxyRouteTarget::Full(cfg) = target {
            if let Some(cache) = &cfg.cache {
                if cache.store.starts_with("redis://") || cache.store.starts_with("rediss://") {
                    urls.insert(cache.store.clone());
                }
            }
        }
    };

    for site in &config.sites {
        if let Some(ProxyConfig::Routes(routes)) = &site.proxy {
            routes.values().for_each(&mut push_if_redis);
        }
        if let Some(routes) = &site.routes {
            for route in routes {
                if let Some(target) = &route.proxy {
                    push_if_redis(target);
                }
            }
        }
    }

    urls.into_iter().collect()
}

/// Establish every Redis cache connection named by `config` up front, so
/// the request path never has to connect from inside Pingora's own runtime
/// (issue #330).
///
/// Fail-open: individual connect failures are logged by
/// [`conduit_cache::redis::connect_and_register`] and skipped — an
/// unreachable cache must never block startup or a reload. Idempotent
/// (already-registered URLs are a cheap no-op), so this is safe to call on
/// every reload, not just once at startup.
#[cfg(feature = "cache")]
pub(crate) async fn connect_all(config: &AppConfig) {
    for url in collect_redis_cache_urls(config) {
        conduit_cache::redis::connect_and_register(&url).await;
    }
}

#[cfg(all(test, feature = "cache"))]
mod tests {
    use super::*;

    fn cfg(json: &str) -> AppConfig {
        crate::config::parse::from_str(json).expect("valid config")
    }

    #[test]
    fn finds_url_under_proxy_shorthand() {
        let config = cfg(
            r#"{"sites":[{"port":8080,"proxy":{"/":{"targets":["http://up:1"],
                "cache":{"store":"redis://127.0.0.1:6379","ttlSecs":60}}}}]}"#,
        );
        assert_eq!(
            collect_redis_cache_urls(&config),
            vec!["redis://127.0.0.1:6379".to_owned()]
        );
    }

    #[test]
    fn finds_url_under_routes_array() {
        let config = cfg(r#"{"sites":[{"port":8080,"routes":[{"match":{"path":"/"},
                "proxy":{"targets":["http://up:1"],
                "cache":{"store":"redis://127.0.0.1:6380","ttlSecs":60}}}]}]}"#);
        assert_eq!(
            collect_redis_cache_urls(&config),
            vec!["redis://127.0.0.1:6380".to_owned()]
        );
    }

    #[test]
    fn deduplicates_one_url_shared_by_two_routes() {
        let config = cfg(r#"{"sites":[{"port":8080,"proxy":{
                "/a":{"targets":["http://up:1"],"cache":{"store":"redis://127.0.0.1:6379","ttlSecs":60}},
                "/b":{"targets":["http://up:2"],"cache":{"store":"redis://127.0.0.1:6379","ttlSecs":60}}
            }}]}"#);
        assert_eq!(
            collect_redis_cache_urls(&config),
            vec!["redis://127.0.0.1:6379".to_owned()]
        );
    }

    #[test]
    fn two_distinct_urls_both_returned_in_sorted_order() {
        let config = cfg(r#"{"sites":[{"port":8080,"proxy":{
                "/a":{"targets":["http://up:1"],"cache":{"store":"redis://z-host:6379","ttlSecs":60}},
                "/b":{"targets":["http://up:2"],"cache":{"store":"redis://a-host:6379","ttlSecs":60}}
            }}]}"#);
        assert_eq!(
            collect_redis_cache_urls(&config),
            vec![
                "redis://a-host:6379".to_owned(),
                "redis://z-host:6379".to_owned(),
            ]
        );
    }

    #[test]
    fn accepts_rediss_tls_scheme() {
        let config = cfg(
            r#"{"sites":[{"port":8080,"proxy":{"/":{"targets":["http://up:1"],
                "cache":{"store":"rediss://secure-host:6380","ttlSecs":60}}}}]}"#,
        );
        assert_eq!(
            collect_redis_cache_urls(&config),
            vec!["rediss://secure-host:6380".to_owned()]
        );
    }

    #[test]
    fn skips_memory_disk_and_unsupported_stores() {
        let config = cfg(r#"{"sites":[{"port":8080,"proxy":{
                "/a":{"targets":["http://up:1"],"cache":{"store":"memory","ttlSecs":60}},
                "/b":{"targets":["http://up:2"],"cache":{"store":"disk:/var/cache","ttlSecs":60}},
                "/c":{"targets":["http://up:3"],"cache":{"store":"s3://bucket","ttlSecs":60}}
            }}]}"#);
        assert!(collect_redis_cache_urls(&config).is_empty());
    }

    #[test]
    fn empty_for_single_string_proxy_and_route_without_cache() {
        let config = cfg(r#"{"sites":[
                {"port":8080,"proxy":"http://up:1"},
                {"port":8081,"proxy":{"/":{"targets":["http://up:2"]}}}
            ]}"#);
        assert!(collect_redis_cache_urls(&config).is_empty());
    }
}
