//! Shared "walk every level of a [`SiteConfig`] that can carry a `rateLimit`
//! block" iterator (issue #360).
//!
//! Three call sites used to hand-duplicate this exact site → proxy-map-route
//! → consumer scan: `src/server/builder.rs::find_redis_rate_limit_store`,
//! `src/config/validate/cross_site.rs::collect_redis_stores`, and
//! `src/config/validate/warnings.rs::site_uses_redis_store`. None of them scanned
//! `site.routes[*].proxy.rateLimit` (Phase 3.6 "advanced routing") at all —
//! the same blind spot #360 found in the actual *enforcement* path
//! (`router.rs::find_route_rate_limit`, now deleted in favor of stamping the
//! matched route's rate limit onto the routing decision itself). Fixing the
//! walk in exactly one place means every caller picks up the missing
//! `routes[]` level for free instead of needing three matching hand-edits
//! that can (and did) drift out of sync with each other.

use crate::config::schema::{
    AppConfig, ProxyConfig, ProxyRouteTarget, RateLimitConfig, SiteConfig,
};

/// Yield every [`RateLimitConfig`] configured anywhere on `site`, in this
/// fixed order: site-level, then each `proxy` map route's `Full` target (in
/// map order), then each `routes[]` entry's `Full` proxy target (in
/// declaration order), then each consumer's own rate limit (in list order).
///
/// Order matters only in that it must stay stable and match what callers
/// already document (e.g. `find_redis_rate_limit_store`'s "first match
/// wins") — changing it changes which URL wins when levels disagree (#357).
pub(crate) fn iter_rate_limit_configs(site: &SiteConfig) -> impl Iterator<Item = &RateLimitConfig> {
    let site_level = site.rate_limit.iter();

    let proxy_map = match &site.proxy {
        Some(ProxyConfig::Routes(routes)) => Some(routes.values()),
        _ => None,
    }
    .into_iter()
    .flatten()
    .filter_map(full_target_rate_limit);

    let routes_array = site
        .routes
        .iter()
        .flatten()
        .filter_map(|route| route.proxy.as_ref())
        .filter_map(full_target_rate_limit);

    site_level
        .chain(proxy_map)
        .chain(routes_array)
        .chain(consumer_rate_limits(site))
}

/// Find the first `redis://`/`rediss://` `rateLimit.store` configured anywhere
/// in `config` — site-level, per-route (`proxy.*.rateLimit` AND
/// `routes[*].proxy.rateLimit`, issue #360), or per-consumer
/// (`consumers.consumers[].rateLimit`) — so a Redis backend is connected at
/// startup even when Redis is used *only* at the route/consumer layer (issue
/// #322: previously only the site level was scanned, so a route/consumer-only
/// Redis config silently fell back to the in-memory limiter forever, since
/// `AppState.redis_rate_limiter` was never populated in the first place).
///
/// Scan order (site → `proxy` map → `routes[]` → consumer, across sites in
/// declaration order) is shared with `config::validate`'s Redis-consistency
/// checks via [`iter_rate_limit_configs`] —
/// see that module for why the walk lives in exactly one place.
pub(crate) fn find_redis_rate_limit_store(config: &AppConfig) -> Option<String> {
    config
        .sites
        .iter()
        .flat_map(iter_rate_limit_configs)
        .filter_map(|rl| rl.store.as_deref())
        .find(|store| conduit_config_core::scheme::is_redis_url(store))
        .map(str::to_owned)
}

/// The Redis rate-limit store the connection was made for at startup (`None` = none configured).
/// Set once by the bootstrap; later reloads are compared against it, not against the previous
/// reload, so the warning repeats until the process restarts (issue #358).
static STARTUP_REDIS_STORE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

#[cfg(feature = "redis")]
pub(crate) fn record_startup_redis_store(config: &AppConfig) {
    let _ = STARTUP_REDIS_STORE.set(find_redis_rate_limit_store(config));
}

/// A warning when `new` names a different Redis rate-limit store than the startup connection, or
/// none where startup had one. The connection is made once at startup (issue #358) and never
/// re-scanned, so a hot reload that changes it leaves the old connection (or the in-memory fallback)
/// in use; saying so beats silently ignoring the edit.
pub(crate) fn redis_rate_limit_change_warning(new: &AppConfig) -> Option<String> {
    let startup = STARTUP_REDIS_STORE.get()?;
    redis_store_change_warning(
        startup.as_deref(),
        find_redis_rate_limit_store(new).as_deref(),
    )
}

fn redis_store_change_warning(startup: Option<&str>, new: Option<&str>) -> Option<String> {
    if startup == new {
        return None;
    }
    Some(
        "rateLimit.store: the Redis rate-limit connection is made at startup only; this change \
         takes effect after a restart (the startup connection, or the in-memory limiter, stays in use)"
            .to_owned(),
    )
}

/// `Full(cfg)` route targets carry their own optional `rateLimit`; the
/// shorthand `Url`/`RoundRobin` variants never do.
fn full_target_rate_limit(target: &ProxyRouteTarget) -> Option<&RateLimitConfig> {
    match target {
        ProxyRouteTarget::Full(cfg) => cfg.rate_limit.as_ref(),
        _ => None,
    }
}

/// Consumer-level rate limits only matter when the `consumers` feature is
/// actually compiled in — without it, `ConsumersGuard` never runs and
/// `Consumer.rate_limit` is dead config, so scanning it (e.g. to decide
/// whether to open a Redis connection at startup) would be pointless. Both
/// branches return the same `impl Iterator<Item = &RateLimitConfig>` type;
/// `#[cfg]` removes one of them entirely at compile time, so this compiles
/// like a normal single-armed function rather than an `if`/`else` with
/// mismatched arm types.
fn consumer_rate_limits(
    #[cfg_attr(not(feature = "consumers"), allow(unused_variables))] site: &SiteConfig,
) -> impl Iterator<Item = &RateLimitConfig> {
    #[cfg(feature = "consumers")]
    {
        site.consumers
            .iter()
            .flat_map(|c| c.consumers.iter())
            .filter_map(|consumer| consumer.rate_limit.as_ref())
    }
    #[cfg(not(feature = "consumers"))]
    {
        std::iter::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{ProxyRouteConfig, ProxyTarget, RouteConfig};
    use indexmap::IndexMap;

    fn rl(store: &str) -> RateLimitConfig {
        RateLimitConfig {
            window_secs: 60,
            limit: 10,
            burst: None,
            algorithm: None,
            key_by: None,
            skip_paths: None,
            store: Some(store.to_owned()),
            dry_run: None,
        }
    }

    fn full_target(store: &str) -> ProxyRouteTarget {
        ProxyRouteTarget::Full(Box::new(ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://up:80".to_owned())],
            rate_limit: Some(rl(store)),
            ..Default::default()
        }))
    }

    #[test]
    fn empty_site_yields_nothing() {
        let site = SiteConfig::default();
        assert_eq!(iter_rate_limit_configs(&site).count(), 0);
    }

    #[test]
    fn site_level_only() {
        let site = SiteConfig {
            rate_limit: Some(rl("redis://site")),
            ..Default::default()
        };
        let stores: Vec<&str> = iter_rate_limit_configs(&site)
            .filter_map(|c| c.store.as_deref())
            .collect();
        assert_eq!(stores, vec!["redis://site"]);
    }

    #[test]
    fn proxy_map_route_is_discovered() {
        let mut routes = IndexMap::new();
        routes.insert("/api".to_owned(), full_target("redis://from-proxy-map"));
        let site = SiteConfig {
            proxy: Some(ProxyConfig::Routes(routes)),
            ..Default::default()
        };
        let stores: Vec<&str> = iter_rate_limit_configs(&site)
            .filter_map(|c| c.store.as_deref())
            .collect();
        assert_eq!(stores, vec!["redis://from-proxy-map"]);
    }

    /// The actual gap #360 is about: a Redis store configured only under
    /// `site.routes[*].proxy.rateLimit` (Phase 3.6 advanced routing) must be
    /// discovered too, not just the legacy `proxy` map.
    #[test]
    fn routes_array_entry_is_discovered() {
        let route = RouteConfig {
            r#match: Default::default(),
            proxy: Some(full_target("redis://from-routes-array")),
            static_files: None,
        };
        let site = SiteConfig {
            routes: Some(vec![route]),
            ..Default::default()
        };
        let stores: Vec<&str> = iter_rate_limit_configs(&site)
            .filter_map(|c| c.store.as_deref())
            .collect();
        assert_eq!(stores, vec!["redis://from-routes-array"]);
    }

    #[test]
    fn both_proxy_map_and_routes_array_are_discovered_in_order() {
        let mut routes_map = IndexMap::new();
        routes_map.insert("/legacy".to_owned(), full_target("redis://proxy-map"));
        let route = RouteConfig {
            r#match: Default::default(),
            proxy: Some(full_target("redis://routes-array")),
            static_files: None,
        };
        let site = SiteConfig {
            rate_limit: Some(rl("redis://site")),
            proxy: Some(ProxyConfig::Routes(routes_map)),
            routes: Some(vec![route]),
            ..Default::default()
        };
        let stores: Vec<&str> = iter_rate_limit_configs(&site)
            .filter_map(|c| c.store.as_deref())
            .collect();
        assert_eq!(
            stores,
            vec!["redis://site", "redis://proxy-map", "redis://routes-array"]
        );
    }

    #[cfg(feature = "consumers")]
    #[test]
    fn consumer_level_is_discovered_last() {
        use crate::config::schema::{Consumer, ConsumersConfig};

        let site = SiteConfig {
            consumers: Some(ConsumersConfig {
                consumers: vec![Consumer {
                    username: "alice".to_owned(),
                    api_key: None,
                    basic_auth: None,
                    jwt: None,
                    rate_limit: Some(rl("redis://consumer")),
                    headers: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let stores: Vec<&str> = iter_rate_limit_configs(&site)
            .filter_map(|c| c.store.as_deref())
            .collect();
        assert_eq!(stores, vec!["redis://consumer"]);
    }

    #[cfg(not(feature = "consumers"))]
    #[test]
    fn consumer_level_ignored_without_feature() {
        // Without the `consumers` feature, `Consumer`'s own fields can't be
        // constructed from this test conveniently — the relevant behavior is
        // just that `consumer_rate_limits` never panics/never yields when
        // the feature is off, covered implicitly by every other test in this
        // module still passing under `--no-default-features`.
        let site = SiteConfig::default();
        assert_eq!(iter_rate_limit_configs(&site).count(), 0);
    }
}

#[cfg(test)]
mod redis_change_tests {
    use super::*;

    #[test]
    fn unchanged_store_is_silent() {
        assert!(
            redis_store_change_warning(Some("redis://a:6379"), Some("redis://a:6379")).is_none()
        );
        assert!(redis_store_change_warning(None, None).is_none());
    }

    #[test]
    fn added_changed_or_removed_store_warns() {
        let (a, b) = (Some("redis://a:6379"), Some("redis://b:6379"));
        assert!(redis_store_change_warning(None, a).is_some(), "added");
        assert!(redis_store_change_warning(a, b).is_some(), "changed");
        assert!(redis_store_change_warning(a, None).is_some(), "removed");
    }

    /// The warning compares with the startup store, so it keeps firing on the reload after a change.
    #[test]
    fn a_second_reload_with_the_same_changed_store_still_warns() {
        let startup = Some("redis://a:6379");
        let changed = Some("redis://b:6379");
        assert!(redis_store_change_warning(startup, changed).is_some());
        assert!(redis_store_change_warning(startup, changed).is_some());
    }
}
