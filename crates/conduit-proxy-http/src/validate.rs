//! Config validation for `proxy`/`routes[]`: proxy targets, route configs, caches, upstream groups and rewrites. Called by the root
//! crate's `config::validate`.

use conduit_cache::validate::validate_cache_config;
use conduit_config_core::validation::ValidationError;
use conduit_ratelimit::validate::validate_rate_limit;
use conduit_upstream::{LoadBalanceStrategy, ProxyTarget, UpstreamGroup};

use crate::config::{ProxyConfig, ProxyRouteConfig, ProxyRouteTarget, RewriteRule};

pub fn validate_proxy(proxy: &ProxyConfig, prefix: &str, errors: &mut Vec<ValidationError>) {
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

pub fn validate_route_config(
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
    warn_slow_start_ignored(cfg, prefix);
    if let Some(cache) = &cfg.cache {
        validate_cache_config(cache, &format!("{prefix}.cache"), errors);
    }
    // Per-route rateLimit was never validated at all before issue #310 (found
    // independently by security-engineer and CodeRabbit reviewing #309) — a
    // malformed keyBy/zero windowSecs/unknown algorithm/invalid store all
    // passed silently. Now shares the same validation as site/consumer level.
    if let Some(rate_limit) = &cfg.rate_limit {
        validate_rate_limit(rate_limit, prefix, errors);
        // `RateLimitConfig` is shared with the site and consumer levels, which honour `dryRun`; the per-route check
        // never reads it, so accepting it here would silently enforce the limit (#521). The JSON Schema already omits it.
        if rate_limit.dry_run.is_some() {
            errors.push(ValidationError::new(
                format!("{prefix}.rateLimit.dryRun"),
                "dryRun is not supported on a route-level rateLimit (only site and consumer rate limits support it)",
            ));
        }
    }
}

/// Warn (never an error) when `healthCheck.slowStartSecs` is set on a route where it has no effect.
///
/// Slow start (#157) is deliberately not applied to hash-based strategies
/// or sticky sessions -- a client's hash/pin must map to a fixed upstream
/// for the strategy's own consistency guarantee to hold, which a
/// probabilistic ramp gate would break. Warn rather than silently ignore,
/// so an operator isn't left believing a recovered upstream on this route
/// is being ramped when it isn't. Per-route, not sitewide: other routes on
/// the same site ramp normally.
///
/// Split out of `validate_route_config` (Sonar `rust:S3776`); it only logs, so the order of the
/// validation errors is unchanged.
fn warn_slow_start_ignored(cfg: &ProxyRouteConfig, prefix: &str) {
    let Some(window) = cfg.health_check.as_ref().and_then(|h| h.slow_start_secs) else {
        return;
    };
    if window == 0 {
        return;
    }
    // A route with `groups` never reads its own `strategy` or `sticky` (`resolve_grouped` picks the
    // group by `groupStrategy` and the target by each group's strategy; groups have no sticky
    // sessions), so judging it by them would warn about a leftover field that changes nothing
    // (issue #483). The groups are judged below.
    let has_groups = cfg.groups.as_ref().is_some_and(|g| !g.is_empty());
    let hash_based = matches!(
        cfg.strategy,
        Some(LoadBalanceStrategy::IpHash | LoadBalanceStrategy::ConsistentHash)
    );
    if !has_groups && (hash_based || cfg.sticky.is_some()) {
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
    for group in cfg.groups.iter().flatten() {
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

/// Validate upstream groups: non-empty targets and WRR strategy requirements.
fn validate_groups_config(
    groups: &[UpstreamGroup],
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

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use conduit_upstream::config::UpstreamHealthCheck;
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;
    use crate::config::StickyConfig;

    /// A `tracing` writer that keeps everything it is given.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    /// Everything `validate_route_config` logs at warn level for `cfg`, one line per warning.
    fn warnings_for(cfg: &ProxyRouteConfig) -> Vec<String> {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut errors = Vec::new();
            validate_route_config(cfg, "sites[0].proxy", &mut errors);
        });
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        text.lines().map(str::to_owned).collect()
    }

    fn route(
        strategy: Option<LoadBalanceStrategy>,
        slow_start_secs: Option<u64>,
    ) -> ProxyRouteConfig {
        ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://a:1".to_owned())],
            strategy,
            health_check: slow_start_secs.map(|s| UpstreamHealthCheck {
                slow_start_secs: Some(s),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn route_dry_run_errors(dry_run: Option<bool>) -> Vec<String> {
        let cfg = ProxyRouteConfig {
            targets: vec![ProxyTarget::Simple("http://a:1".to_owned())],
            rate_limit: Some(conduit_ratelimit::RateLimitConfig {
                window_secs: 60,
                limit: 10,
                burst: None,
                algorithm: None,
                key_by: None,
                skip_paths: None,
                store: None,
                dry_run,
            }),
            ..Default::default()
        };
        let mut errors = Vec::new();
        validate_route_config(&cfg, "sites[0].proxy", &mut errors);
        errors.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn route_level_dry_run_is_rejected_not_silently_ignored() {
        for dry_run in [Some(true), Some(false)] {
            let errors = route_dry_run_errors(dry_run);
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert_eq!(errors[0], "sites[0].proxy.rateLimit.dryRun");
        }
        assert!(route_dry_run_errors(None).is_empty());
    }

    fn group(name: &str, strategy: Option<LoadBalanceStrategy>) -> UpstreamGroup {
        UpstreamGroup {
            name: name.to_owned(),
            targets: vec![ProxyTarget::Simple("http://a:1".to_owned())],
            strategy,
        }
    }

    #[test]
    fn slow_start_on_a_hash_based_route_warns_once() {
        for strategy in [
            LoadBalanceStrategy::IpHash,
            LoadBalanceStrategy::ConsistentHash,
        ] {
            let warnings = warnings_for(&route(Some(strategy), Some(30)));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(
                warnings[0]
                    .contains("sites[0].proxy.healthCheck.slowStartSecs is ignored on this route"),
                "{warnings:?}"
            );
        }
    }

    #[test]
    fn slow_start_on_a_sticky_route_warns_once() {
        let mut cfg = route(Some(LoadBalanceStrategy::RoundRobin), Some(30));
        cfg.sticky = Some(StickyConfig {
            cookie: "sid".to_owned(),
            secret: None,
            strict: None,
        });
        let warnings = warnings_for(&cfg);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("is ignored on this route"),
            "{warnings:?}"
        );
    }

    #[test]
    fn slow_start_on_a_hash_based_group_warns_for_that_group_only() {
        let mut cfg = route(Some(LoadBalanceStrategy::RoundRobin), Some(30));
        cfg.groups = Some(vec![
            group("plain", Some(LoadBalanceStrategy::RoundRobin)),
            group("pinned", Some(LoadBalanceStrategy::IpHash)),
            group("default", None),
        ]);
        let warnings = warnings_for(&cfg);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("is ignored for group 'pinned'"),
            "{warnings:?}"
        );
    }

    /// Issue #483: with `groups`, the route-level `strategy` / `sticky` are not used, so only the
    /// groups are judged — a leftover `strategy: ip-hash` must not produce a false route warning.
    #[test]
    fn a_route_with_groups_is_judged_by_its_groups_not_its_own_strategy() {
        let mut cfg = route(Some(LoadBalanceStrategy::IpHash), Some(5));
        cfg.sticky = Some(StickyConfig {
            cookie: "sid".to_owned(),
            secret: None,
            strict: None,
        });
        cfg.groups = Some(vec![group("plain", Some(LoadBalanceStrategy::RoundRobin))]);
        assert!(warnings_for(&cfg).is_empty());

        cfg.groups = Some(vec![group(
            "pinned",
            Some(LoadBalanceStrategy::ConsistentHash),
        )]);
        cfg.group_strategy = Some(LoadBalanceStrategy::IpHash);
        let warnings = warnings_for(&cfg);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("is ignored for group 'pinned'"),
            "{warnings:?}"
        );
    }

    #[test]
    fn no_slow_start_warning_when_it_is_off_or_applies() {
        // window 0, and no health check at all: nothing is ignored.
        assert!(warnings_for(&route(Some(LoadBalanceStrategy::IpHash), Some(0))).is_empty());
        assert!(warnings_for(&route(Some(LoadBalanceStrategy::IpHash), None)).is_empty());
        // a strategy the ramp does apply to.
        assert!(warnings_for(&route(Some(LoadBalanceStrategy::RoundRobin), Some(30))).is_empty());
        assert!(warnings_for(&route(None, Some(30))).is_empty());
        // a hash-based group under window 0 stays quiet too.
        let mut cfg = route(None, Some(0));
        cfg.groups = Some(vec![group("pinned", Some(LoadBalanceStrategy::IpHash))]);
        assert!(warnings_for(&cfg).is_empty());
    }
}
