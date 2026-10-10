//! `required_features()`: the Cargo features a configuration needs, read from the same predicates
//! `feature_warnings()` uses (issue #473).
//!
//! `feature_warnings()` says "this config uses X but this build lacks the feature"; this module answers
//! the opposite question — which features would a build need so that it says nothing. The predicate for
//! each feature is the one its crate's `warnings::feature_warning` takes (`Option::is_some` on the
//! config block, or the shared `site_uses_redis_store` / `site_has_cache_config` helpers), and
//! `compiled_features()` reads each crate's own `COMPILED` const, so a test can check that the two
//! views of one configuration agree (`tests` below).

use crate::config::schema::{AppConfig, SiteConfig};

use super::warnings::{site_has_cache_config, site_uses_redis_store};

/// Every optional root feature a configuration can need, in the order they are printed.
pub const FEATURE_ORDER: &[&str] = &[
    "proxy",
    "cache",
    "redis",
    "static",
    "compression",
    "hotreload",
    "jwt",
    "consumers",
    "forward-auth",
    "acme",
    "tcp",
    "upload",
    "fault-injection",
    "rhai",
    "wasm",
    "otlp",
];

/// Features this build was compiled with, among [`FEATURE_ORDER`].
pub fn compiled_features() -> Vec<&'static str> {
    let flags: [(&str, bool); 16] = [
        ("proxy", conduit_proxy_http::warnings::COMPILED),
        ("cache", conduit_cache::warnings::COMPILED),
        ("redis", conduit_ratelimit::warnings::COMPILED),
        ("static", conduit_static::warnings::COMPILED),
        ("compression", conduit_compression::warnings::COMPILED),
        ("hotreload", conduit_hotreload::warnings::COMPILED),
        ("jwt", conduit_auth_jwt::warnings::COMPILED),
        ("consumers", conduit_auth_consumers::warnings::COMPILED),
        ("forward-auth", conduit_auth_forward::warnings::COMPILED),
        ("acme", conduit_acme::warnings::COMPILED),
        ("tcp", conduit_tcp::warnings::COMPILED),
        ("upload", conduit_upload::warnings::COMPILED),
        ("fault-injection", conduit_faults::warnings::COMPILED),
        ("rhai", conduit_middleware::warnings::RHAI_COMPILED),
        ("wasm", conduit_middleware::warnings::WASM_COMPILED),
        ("otlp", conduit_otlp::warnings::COMPILED),
    ];
    flags
        .into_iter()
        .filter_map(|(name, on)| on.then_some(name))
        .collect()
}

/// The minimal set of root features `config` needs, in [`FEATURE_ORDER`]. A feature another listed one
/// already pulls in is left out (`cache` enables `proxy`), so the result is the shortest
/// `--features` list.
pub fn required_features(config: &AppConfig) -> Vec<&'static str> {
    let mut needed = [false; FEATURE_ORDER.len()];
    let mut need = |name: &str| {
        let i = FEATURE_ORDER
            .iter()
            .position(|f| *f == name)
            .expect("feature listed in FEATURE_ORDER");
        needed[i] = true;
    };

    if config
        .global
        .as_ref()
        .and_then(|g| g.otlp.as_ref())
        .is_some()
    {
        need("otlp");
    }
    for site in &config.sites {
        site_requires(site, &mut need);
    }

    // `cache` pulls `proxy` in (root Cargo.toml: `cache = ["proxy", ...]`).
    let implied = |name: &str| name == "proxy" && needed[1];
    FEATURE_ORDER
        .iter()
        .zip(needed)
        .filter(|(name, on)| *on && !implied(name))
        .map(|(name, _)| *name)
        .collect()
}

fn site_requires(site: &SiteConfig, need: &mut impl FnMut(&str)) {
    let route_proxy = site
        .routes
        .iter()
        .flatten()
        .any(|route| route.proxy.is_some());
    if site.proxy.is_some() || route_proxy {
        need("proxy");
    }
    if site_has_cache_config(site) {
        need("cache");
    }
    if site_uses_redis_store(site) {
        need("redis");
    }
    if site.static_files.is_some() || site.fallback.is_some() {
        need("static");
    }
    if site.compression.is_some() {
        need("compression");
    }
    if site.hot_reload.is_some() {
        need("hotreload");
    }
    let consumers_use_jwt = site.consumers.as_ref().is_some_and(|c| {
        c.shared_jwt.is_some() || c.consumers.iter().any(|consumer| consumer.jwt.is_some())
    });
    if site.jwt_auth.is_some() || consumers_use_jwt {
        need("jwt");
    }
    if site.consumers.is_some() {
        need("consumers");
    }
    if site.forward_auth.is_some() {
        need("forward-auth");
    }
    if site.tls.as_ref().and_then(|t| t.acme.as_ref()).is_some() {
        need("acme");
    }
    if site.tcp.is_some() {
        need("tcp");
    }
    if site.upload.is_some() {
        need("upload");
    }
    if site.fault_injection.is_some() {
        need("fault-injection");
    }
    for entry in site.middleware.iter().flatten() {
        match entry.r#type.as_str() {
            "script" => need("rhai"),
            "wasm" => need("wasm"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: serde_json::Value) -> AppConfig {
        crate::config::from_str(&json.to_string()).expect("test config parses")
    }

    #[test]
    fn a_plain_config_needs_nothing() {
        let cfg = config(serde_json::json!({ "sites": [{ "port": 8080 }] }));
        assert!(required_features(&cfg).is_empty());
    }

    #[test]
    fn cache_implies_proxy_so_proxy_is_not_listed() {
        let cfg = config(serde_json::json!({ "sites": [{
            "port": 8080,
            "proxy": { "/api": { "targets": ["http://127.0.0.1:1"], "cache": { "store": "memory", "ttlSecs": 60 } } }
        }] }));
        assert_eq!(required_features(&cfg), vec!["cache"]);
    }

    #[test]
    fn proxy_without_cache_lists_proxy() {
        let cfg = config(serde_json::json!({ "sites": [{
            "port": 8080,
            "proxy": { "/api": "http://127.0.0.1:1" }
        }] }));
        assert_eq!(required_features(&cfg), vec!["proxy"]);
    }

    #[test]
    fn features_come_out_in_the_fixed_order_without_duplicates() {
        let cfg = config(serde_json::json!({ "sites": [
            { "port": 8080, "compression": true, "static": "./dist", "forwardAuth": { "url": "http://127.0.0.1:1/auth" } },
            { "port": 8081, "static": "./other", "compression": true }
        ] }));
        assert_eq!(
            required_features(&cfg),
            vec!["static", "compression", "forward-auth"]
        );
    }

    /// The two views of one configuration agree: a feature is missing from this build exactly when
    /// `feature_warnings()` names it. Run in every CI feature cell, so a new `feature_warning` whose
    /// predicate is not mirrored in `required_features()` (or the reverse) fails here.
    #[test]
    fn required_features_agree_with_feature_warnings_for_every_example() {
        let compiled = compiled_features();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("examples dir") {
            let path = entry.expect("dir entry").path();
            let ext = path.extension().and_then(|e| e.to_str());
            if !matches!(ext, Some("json" | "yaml" | "yml")) {
                continue;
            }
            let Ok(cfg) = crate::config::load_config(&path) else {
                continue; // not a Conduit config (a fragment or a schema example)
            };
            let required = required_features(&cfg);
            let missing: Vec<&str> = required
                .iter()
                .copied()
                .filter(|f| !compiled.contains(f))
                .collect();
            let warnings = super::super::feature_warnings(&cfg);
            // The features the warnings name, exactly: every feature-off warning ends in
            // "Recompile with `--features X` to enable."
            let named: Vec<&str> = warnings
                .iter()
                .filter_map(|w| {
                    let rest = w.split("`--features ").nth(1)?;
                    rest.split('`').next()
                })
                .collect();
            for feature in &missing {
                // Consumers that use JWT need `jwt`, but the "consumers need jwt" warning only fires
                // in a build that has `consumers`; without it the `consumers` warning is the one shown.
                if *feature == "jwt" && missing.contains(&"consumers") {
                    continue;
                }
                assert!(
                    named.contains(feature),
                    "{}: `{feature}` is required but missing, yet feature_warnings() does not name it: {warnings:?}",
                    path.display()
                );
            }
            // The other direction: a feature the warnings name must be one `required_features` reports,
            // or `conduit features` would exit 0 for a config that the server warns about.
            for feature in &named {
                assert!(
                    required.contains(feature) || (*feature == "proxy" && required.contains(&"cache")),
                    "{}: feature_warnings() names `{feature}` but required_features() does not report it ({required:?})",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked > 0, "no example configs were read from {dir:?}");
    }
}
