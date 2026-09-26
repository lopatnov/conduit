//! Validation of `proxy`/`routes[]` proxy targets, route configs, caches, upstream groups and rewrites.

use super::ValidationError;

use conduit_cache::validate::validate_cache_config;
use conduit_ratelimit::validate::validate_rate_limit;

use crate::config::schema::{
    LoadBalanceStrategy, ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, ProxyTarget, RewriteRule,
};

pub(super) fn validate_proxy(proxy: &ProxyConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
    match proxy {
        ProxyConfig::Single(url) => {
            if !is_valid_upstream_url(url) {
                errors.push(ValidationError::new(
                    prefix,
                    format!("Invalid upstream URL '{url}' — must start with http:// or https://"),
                ));
            }
        }
        ProxyConfig::Routes(routes) => {
            for (route, target) in routes {
                let route_prefix = format!("{prefix}[\"{route}\"]");
                validate_proxy_route_target(target, &route_prefix, errors);
            }
        }
    }
}

fn validate_proxy_route_target(
    target: &ProxyRouteTarget,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    match target {
        ProxyRouteTarget::Url(url) => {
            if !is_valid_upstream_url(url) {
                errors.push(ValidationError::new(
                    prefix,
                    format!("Invalid upstream URL '{url}' — must start with http:// or https://"),
                ));
            }
        }
        ProxyRouteTarget::RoundRobin(urls) => {
            for (i, url) in urls.iter().enumerate() {
                if !is_valid_upstream_url(url) {
                    errors.push(ValidationError::new(
                        format!("{prefix}[{i}]"),
                        format!(
                            "Invalid upstream URL '{url}' — must start with http:// or https://"
                        ),
                    ));
                }
            }
        }
        ProxyRouteTarget::Full(cfg) => {
            validate_route_config(cfg, prefix, errors);
        }
    }
}

/// Return `true` when the string is an absolute http:// or https:// URL with a non-empty host.
fn is_valid_upstream_url(url: &str) -> bool {
    let rest = if let Some(r) = url.strip_prefix("http://") {
        r
    } else if let Some(r) = url.strip_prefix("https://") {
        r
    } else {
        return false;
    };
    // After stripping the scheme the host must be non-empty.
    !rest.split('/').next().unwrap_or("").is_empty()
}

pub(super) fn validate_route_config(
    cfg: &ProxyRouteConfig,
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    if cfg.targets.is_empty() && cfg.groups.is_none() {
        errors.push(ValidationError::new(
            format!("{prefix}.targets"),
            "At least one target is required (or use 'groups' for two-level balancing)",
        ));
    }
    if let Some(groups) = &cfg.groups {
        validate_groups_config(groups, prefix, errors);
    }
    if cfg.strategy == Some(LoadBalanceStrategy::WeightedRoundRobin) {
        check_weighted_targets(&cfg.targets, &format!("{prefix}.targets"), errors);
    }
    validate_target_urls(&cfg.targets, prefix, errors);
    if let Some(rules) = &cfg.rewrite {
        validate_rewrite_rules(rules, prefix, errors);
    }
    if let Some(mirror) = &cfg.mirror {
        if !mirror.starts_with("http://") && !mirror.starts_with("https://") {
            errors.push(ValidationError::new(
                format!("{prefix}.mirror"),
                "mirror URL must be http:// or https://",
            ));
        }
    }
    if let Some(tls) = &cfg.upstream_tls {
        // Warn if verify: false is set (not an error, just a potential misconfiguration).
        if tls.verify == Some(false) {
            tracing::debug!(
                "{prefix}.upstreamTls.verify is false — upstream certificate will not be verified"
            );
        }
    }
    // Slow start (#157) is deliberately not applied to hash-based strategies
    // or sticky sessions -- a client's hash/pin must map to a fixed upstream
    // for the strategy's own consistency guarantee to hold, which a
    // probabilistic ramp gate would break. Warn rather than silently ignore,
    // so an operator isn't left believing a recovered upstream on this route
    // is being ramped when it isn't. Per-route, not sitewide: other routes on
    // the same site ramp normally.
    if let Some(window) = cfg.health_check.as_ref().and_then(|h| h.slow_start_secs) {
        let hash_based = matches!(
            cfg.strategy,
            Some(LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash)
        );
        if window > 0 && (hash_based || cfg.sticky.is_some()) {
            tracing::warn!(
                "{prefix}.healthCheck.slowStartSecs is ignored on this route: hash-based \
                 strategies and sticky sessions map each client to a fixed upstream, so a \
                 recovered upstream receives full traffic immediately (see docs/configuration.md, \
                 'Slow start')"
            );
        }
        // `groups` selects an *inner* strategy per group (resolve_grouped in
        // router.rs) independent of the route-level `strategy` above -- a
        // hash-based group strategy hits pick_bounded's same early-return and
        // silently bypasses the ramp, with no warning otherwise (found by
        // Gitar reviewing #157/PR #365).
        if window > 0 {
            if let Some(groups) = &cfg.groups {
                for group in groups {
                    if matches!(
                        group.strategy,
                        Some(LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash)
                    ) {
                        tracing::warn!(
                            "{prefix}.healthCheck.slowStartSecs is ignored for group '{}': its \
                             hash-based strategy maps each client to a fixed upstream, so a \
                             recovered upstream receives full traffic immediately (see \
                             docs/configuration.md, 'Slow start')",
                            group.name
                        );
                    }
                }
            }
        }
    }
    if let Some(cache) = &cfg.cache {
        validate_cache_config(cache, &format!("{prefix}.cache"), errors);
    }
    // Per-route rateLimit was never validated at all before issue #310 (found
    // independently by security-engineer and CodeRabbit reviewing #309) — a
    // malformed keyBy/zero windowSecs/unknown algorithm/invalid store all
    // passed silently. Now shares the same validation as site/consumer level.
    if let Some(rate_limit) = &cfg.rate_limit {
        validate_rate_limit(rate_limit, prefix, errors);
    }
}

/// Validate upstream groups: non-empty targets and WRR strategy requirements.
fn validate_groups_config(
    groups: &[crate::config::schema::UpstreamGroup],
    prefix: &str,
    errors: &mut Vec<ValidationError>,
) {
    for (i, group) in groups.iter().enumerate() {
        if group.targets.is_empty() {
            errors.push(ValidationError::new(
                format!("{prefix}.groups[{i}].targets"),
                "At least one target is required in each group",
            ));
        }
        if group.strategy == Some(LoadBalanceStrategy::WeightedRoundRobin) {
            check_weighted_targets(
                &group.targets,
                &format!("{prefix}.groups[{i}].targets"),
                errors,
            );
        }
    }
}

/// Emit an error when any target in `targets` is a `Simple` (unweighted) URL
/// and the strategy is `weighted-round-robin`.
fn check_weighted_targets(targets: &[ProxyTarget], field: &str, errors: &mut Vec<ValidationError>) {
    let has_simple = targets.iter().any(|t| matches!(t, ProxyTarget::Simple(_)));
    if has_simple {
        errors.push(ValidationError::new(
            field.to_string(),
            "Strategy 'weighted-round-robin' requires weighted targets: \
             { \"url\": \"...\", \"weight\": N }",
        ));
    }
}

/// Validate that every target URL starts with `http://` or `https://`.
fn validate_target_urls(targets: &[ProxyTarget], prefix: &str, errors: &mut Vec<ValidationError>) {
    for (i, target) in targets.iter().enumerate() {
        let url = match target {
            ProxyTarget::Simple(u) => u.as_str(),
            ProxyTarget::Weighted(w) => w.url.as_str(),
        };
        if !is_valid_upstream_url(url) {
            errors.push(ValidationError::new(
                format!("{prefix}.targets[{i}]"),
                format!("Invalid upstream URL '{url}' — must start with http:// or https://"),
            ));
        }
    }
}

fn validate_rewrite_rules(rules: &[RewriteRule], prefix: &str, errors: &mut Vec<ValidationError>) {
    for (i, rule) in rules.iter().enumerate() {
        if let Err(e) = regex::Regex::new(&rule.from) {
            errors.push(ValidationError::new(
                format!("{prefix}.rewrite[{i}].from"),
                format!("Invalid regex '{}': {e}", rule.from),
            ));
        }
    }
}
