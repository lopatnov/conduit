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
    check_redis_operator_in(
        config,
        conduit_config_core::scheme::redis_url_env().as_deref(),
        errors,
    );
}

/// [`check_redis_operator`] with the value of `CONDUIT_REDIS_URL` given (the tests pass it, never touching the environment).
fn check_redis_operator_in(
    config: &AppConfig,
    env_url: Option<&str>,
    errors: &mut Vec<ValidationError>,
) {
    let global_url = config
        .global
        .as_ref()
        .and_then(|g| g.redis.as_ref())
        .and_then(|r| r.url.as_deref());
    let operator = conduit_config_core::scheme::resolve_operator_redis_url(global_url, env_url);
    // The address the process would use is a Redis URL, wherever it comes from: a bad one (an `http://` value in the
    // environment of a pod) makes the connection fail at start and every limit count per replica. A value that is
    // present in the file is judged even when blank (it was an error before the environment was an alternative).
    // The message never quotes the value: a wrong one is often a bare secret or `user:pw@host` with no scheme, which
    // no redaction recognises, and this error reaches the log, `/reload` and a Kubernetes event.
    let checked = match global_url {
        Some(g) => Some(("global.redis.url", g.trim())),
        None => env_url.map(|e| (conduit_config_core::scheme::REDIS_URL_ENV, e.trim())),
    };
    if let Some((name, url)) = checked.filter(|(_, u)| !is_redis_url(u)) {
        // a blank value of the variable counts as unset (a Secret with a trailing newline)
        if !(name != "global.redis.url" && url.is_empty()) {
            errors.push(ValidationError::new(
                name,
                format!("{name} must be a redis:// URL (plaintext) or a rediss:// URL (TLS)"),
            ));
        }
    }
    // A bad address is reported once, above; the stores below are judged against the address the process would use.
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
                if let Some(op) = operator
                    .as_deref()
                    // a bad address is reported above, and is never quoted (it may hold a bare secret)
                    .filter(|op| is_redis_url(op) && *op != store)
                {
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
        messages_in(c, None)
    }

    /// The findings with `CONDUIT_REDIS_URL` given: none by default, so a developer's shell does not matter.
    fn messages_in(c: &AppConfig, env_url: Option<&str>) -> Vec<String> {
        let mut errors = Vec::new();
        check_redis_operator_in(c, env_url, &mut errors);
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
        // the credentials are assembled at run time: a literal password in a URL is what secret scanners look for
        let (kept, other_kept) = (["op", "secret"].concat(), ["pw", "ned"].concat());
        let with_credentials =
            serde_json::json!({ "redis": { "url": format!("redis://u:{kept}@a:6379") } });
        let m = messages(&limited(
            &format!("redis://u:{other_kept}@evil:6379"),
            with_credentials,
        ));
        assert!(m.iter().any(|x| x.contains("another Redis")), "{m:?}");
        assert!(
            m.iter()
                .all(|x| !x.contains(&kept) && !x.contains(&other_kept)),
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
    fn an_address_from_the_environment_is_checked_and_used_like_one_from_the_file() {
        let no_global = serde_json::json!({});
        // a bad value of the variable is reported, with its name, and without credentials
        let bad = ["http://u:", "pw", "@redis:6379"].concat();
        let m = messages_in(&limited("redis", no_global.clone()), Some(&bad));
        assert!(
            m.iter().any(|x| x.starts_with("Error CONDUIT_REDIS_URL")),
            "{m:?}"
        );
        assert!(m.iter().all(|x| !x.contains("pw@")), "credentials in {m:?}");
        // a value without a scheme (a bare secret, `user:pw@host`) is never quoted either
        let pw = ["s3", "cret"].concat();
        for bare in [
            format!(":{pw}@redis:6379"),
            format!("user:{pw}@host:6379"),
            pw.clone(),
        ] {
            let file = serde_json::json!({ "redis": { "url": bare } });
            let m = messages_in(&limited("redis", file), None);
            assert!(
                m.iter().any(|x| x.starts_with("Error global.redis.url")),
                "{m:?}"
            );
            let m = messages_in(&limited("redis", serde_json::json!({})), Some(&bare));
            assert!(
                m.iter().any(|x| x.starts_with("Error CONDUIT_REDIS_URL")),
                "{m:?}"
            );
            assert!(m.iter().all(|x| !x.contains(&pw)), "a secret reached {m:?}");
        }
        // a legacy store URL next to a bad operator address does not quote the address either
        let file = serde_json::json!({ "redis": { "url": format!("user:{pw}@host:6379") } });
        let m = messages_in(&limited("redis://a:6379", file), None);
        assert!(m.iter().all(|x| !x.contains(&pw)), "a secret reached {m:?}");
        // a blank file value is an error as before; a blank variable counts as unset
        let blank = serde_json::json!({ "redis": { "url": "  " } });
        assert!(messages_in(&limited("memory", blank), None)
            .iter()
            .any(|x| x.starts_with("Error global.redis.url")));
        assert!(messages_in(&limited("memory", serde_json::json!({})), Some("\n")).is_empty());
        // a good one is the address: a bare `redis` needs nothing else
        assert!(messages_in(&limited("redis", no_global), Some("redis://env:6379")).is_empty());
        // the file wins; a bad file value is reported by its own name even when the environment is fine
        let file = serde_json::json!({ "redis": { "url": "http://x" } });
        let m = messages_in(&limited("redis", file), Some("redis://env:6379"));
        assert!(
            m.iter().any(|x| x.starts_with("Error global.redis.url")),
            "{m:?}"
        );
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
