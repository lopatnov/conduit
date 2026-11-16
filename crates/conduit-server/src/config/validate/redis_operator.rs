//! The Redis address of the process (issue #760): the operator sets it once (`global.redis.url`, or
//! `CONDUIT_REDIS_URL` for a pod that reads `ConduitSite` objects) and sites write `store: redis` without one.

use super::warnings::iter_route_cache_configs;
use super::ValidationError;
use crate::config::schema::AppConfig;
use conduit_config_core::redact::redact_url;
use conduit_config_core::scheme::{is_redis_url, REDIS_STORE};

/// Where a `store` value stands: a bare `redis` needs the operator's address, a legacy URL must be that
/// address when the operator set one.
pub(super) fn check_redis_operator(config: &AppConfig, errors: &mut Vec<ValidationError>) {
    let global_url = config
        .global
        .as_ref()
        .and_then(|g| g.redis.as_ref())
        .and_then(|r| r.url.as_deref());
    if let Some(url) = global_url {
        if !is_redis_url(url.trim()) {
            errors.push(ValidationError::new(
                "global.redis.url",
                "must be a redis:// URL (plaintext) or a rediss:// URL (TLS)",
            ));
        }
    }
    let operator = conduit_config_core::scheme::operator_redis_url_from_env(global_url);
    // A bad `global.redis.url` is reported once, above; the stores below are judged against the address
    // the process would use.
    for (i, site) in config.sites.iter().enumerate() {
        let limits = crate::config::rate_limit_scan::iter_rate_limit_configs(site)
            .filter_map(|rl| rl.store.as_deref())
            .map(|s| ("rateLimit.store", s));
        let caches = iter_route_cache_configs(site).map(|c| ("cache.store", c.store.as_str()));
        for (field, store) in limits.chain(caches) {
            if store == REDIS_STORE {
                if operator.is_none() {
                    errors.push(ValidationError::new(
                        format!("sites[{i}].{field}"),
                        "\"redis\" has no address: set global.redis.url, or the CONDUIT_REDIS_URL \
                         environment variable (a pod that reads ConduitSite objects)",
                    ));
                }
            } else if is_redis_url(store) {
                if let Some(op) = operator.as_deref().filter(|op| *op != store) {
                    errors.push(ValidationError::new(
                        format!("sites[{i}].{field}"),
                        format!(
                            "names another Redis ({}) than the process address ({}): there is one Redis \
                             per process, so write \"redis\" here and set the address once in \
                             global.redis.url",
                            super::warnings::sanitize_for_log(&redact_url(store)),
                            super::warnings::sanitize_for_log(&redact_url(op)),
                        ),
                    ));
                }
            }
        }
    }
}

/// The advisory (logged at start and on reload, not an error: `conduit validate` would fail on a warning) that a
/// `store` written as a `redis://` URL is the old form (issue #760).
pub(super) fn check_redis_legacy_warnings(config: &AppConfig, warnings: &mut Vec<String>) {
    let site_has_url = |site: &crate::config::schema::SiteConfig| {
        crate::config::rate_limit_scan::iter_rate_limit_configs(site)
            .filter_map(|rl| rl.store.as_deref())
            .chain(iter_route_cache_configs(site).map(|c| c.store.as_str()))
            .any(is_redis_url)
    };
    if config.sites.iter().any(site_has_url) {
        warnings.push(
            "a store written as a redis:// URL is deprecated: write `store: redis` and set the address once, \
             in global.redis.url (or CONDUIT_REDIS_URL)"
                .to_owned(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: serde_json::Value) -> AppConfig {
        serde_json::from_value(json).expect("config parses")
    }

    fn messages(c: &AppConfig) -> Vec<String> {
        let mut errors = Vec::new();
        check_redis_operator(c, &mut errors);
        errors
            .iter()
            .map(|e| format!("{:?} {}: {}", e.severity, e.path, e.message))
            .collect()
    }

    fn limited(store: &str, global: serde_json::Value) -> AppConfig {
        config(serde_json::json!({
            "global": global,
            "sites": [{ "port": 8080, "proxy": "http://127.0.0.1:1",
                        "rateLimit": { "windowSecs": 60, "limit": 10, "store": store } }]
        }))
    }

    #[test]
    fn a_bare_redis_needs_an_address() {
        // hermetic: a developer's exported CONDUIT_REDIS_URL must not turn "no address" into an address
        // (nothing in this crate sets it, so removing it cannot race another test)
        std::env::remove_var(conduit_config_core::scheme::REDIS_URL_ENV);
        let none = messages(&limited("redis", serde_json::json!({})));
        assert_eq!(none.len(), 1, "{none:?}");
        assert!(none[0].contains("no address"), "{none:?}");
        let set = serde_json::json!({ "redis": { "url": "redis://redis.internal:6379" } });
        assert!(messages(&limited("redis", set)).is_empty());
    }

    #[test]
    fn a_legacy_url_is_deprecated_and_must_be_the_operators_address() {
        let same = serde_json::json!({ "redis": { "url": "redis://a:6379" } });
        let c = limited("redis://a:6379", same);
        assert!(messages(&c).is_empty(), "the same address is fine");
        let mut warnings = Vec::new();
        check_redis_legacy_warnings(&c, &mut warnings);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("deprecated"), "{warnings:?}");
        let mut none = Vec::new();
        check_redis_legacy_warnings(&limited("redis", serde_json::json!({})), &mut none);
        assert!(none.is_empty());
        let other = serde_json::json!({ "redis": { "url": "redis://a:6379" } });
        let m = messages(&limited("redis://evil:6379", other));
        assert!(
            m.iter()
                .any(|x| x.starts_with("Error") && x.contains("another Redis")),
            "{m:?}"
        );
        // the message names both addresses, never a password (a store URL carries credentials)
        let with_password = serde_json::json!({ "redis": { "url": "redis://u:opsecret@a:6379" } });
        let m = messages(&limited("redis://u:pwned@evil:6379", with_password));
        assert!(m.iter().any(|x| x.contains("another Redis")), "{m:?}");
        assert!(
            m.iter()
                .all(|x| !x.contains("opsecret") && !x.contains("pwned")),
            "a password reached the message: {m:?}"
        );
        // without an operator address a legacy URL still works
        assert!(messages(&limited("redis://a:6379", serde_json::json!({}))).is_empty());
    }

    #[test]
    fn the_operators_address_must_be_a_redis_url() {
        let m = messages(&limited(
            "memory",
            serde_json::json!({ "redis": { "url": "http://x" } }),
        ));
        assert!(m.iter().any(|x| x.contains("global.redis.url")), "{m:?}");
    }

    #[test]
    fn a_cache_store_is_judged_the_same_way() {
        let c = config(serde_json::json!({
            "sites": [{ "port": 8080, "proxy": { "/": { "targets": ["http://127.0.0.1:1"],
                         "cache": { "store": "redis" } } } }]
        }));
        let m = messages(&c);
        assert!(
            m.iter()
                .any(|x| x.contains("sites[0].cache.store") && x.contains("no address")),
            "{m:?}"
        );
    }
}
